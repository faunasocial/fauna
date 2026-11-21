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
  // re-render. No policy logic in the SPA (priority #2). Full-PUT saves start
  // from the persisted snapshot for the fields with no control; a group whose
  // number does not parse (blank included) is refused with the shared reason,
  // never sent with the stored value (admin.md § Errors & edge cases).
  //
  // Read + write are both LIVE. nest rejects an out-of-order spam-threshold write
  // (`fauna.protocol.malformed`); the machine surfaces it via `MailPolicySnapshot::
  // error` → the page's `error-message` (MessageBanner), never faked green.
  // UX/IDs: tests/e2e-unified/ui.yaml `admin-mail` page.
  import { identity } from '#lib/store';
  import { mailPolicyMachine } from '#lib/rpc';
  import {
    ensureWasm,
    parseCount,
    parseCountU64,
    mailKnobsDraftPossibility,
    mailMicrosoftDraftPossibility,
    fcrdnsModeOptions,
    dmarcForensicModeOptions,
    dmarcForensicRetentionWarns,
    imapDeleteNonemptyOptions,
    senderDashboardLinks,
    type ReachPolicyOption,
  } from '#lib/wasm';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { mailHealthCheckStateLabel, secretRotationText } from '#lib/wasm';
  import { mailEnableTogglePossibility, readPossibility } from '#lib/wasm';
  import { localDate, mailHealthHeartbeatText, mailHealthStatusText } from '#lib/value-format';
  import { readOnEntryAndReconnect } from '#lib/admin-reread';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';
  import { afterGate, askPossibility, formGate, formRefusal } from '#lib/form-gate';
  import { dispatchPainted, machineAnswer, reasonAttr, type ReadWord } from '#lib/admin-read-gate';
  import type { Possibility } from '#lib/possibility';

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
  // The deployment-wide DMARC catalog (`put_dmarc_policy`); the page edits four
  // fields, every other record tag round-trips through `...base` on save.
  interface DmarcPolicyView {
    policy_mode: string;
    subdomain_policy_mode: string;
    pct: number;
    adkim_mode: string;
    aspf_mode: string;
    rua_destination: string | null;
    ruf_publish: boolean;
    ruf_destination: string | null;
    fo_mode: string;
    ri_seconds: number;
    aggregate_retention_days: number;
    forensic_retention_days: number;
    outbound_aggregate_enabled: boolean;
  }
  // The Outlook import's admin-supplied Microsoft registration
  // (`set_microsoft_registration`, read back off `fauna.setup.status`).
  interface MicrosoftRegistrationView {
    client_id: string;
    tenant: string;
    is_default: boolean;
  }
  // Outcome of the last PublishSpamBaseline action (null until the admin clicks
  // Publish). Mirrors the shared BaselinePublishView; `published` is the
  // wire flag as the nest sent it.
  interface BaselinePublishView {
    published: boolean;
    contributors: number;
    sample_count: number;
    skipped_contributors: number;
    // A delta-floor deferral also carries `published = false`; check it FIRST.
    deferred: boolean;
  }
  // The served baseline's current state, mirroring the shared BaselineStateView
  // (`null` on the snapshot = blank text: before hydrate, or a nest that refuses
  // the read).
  interface BaselineStateView {
    published: boolean;
    contributors: number;
    sample_count: number;
    published_at_ms: number | null;
    skipped_contributors: number;
    deferred: boolean;
    standing: boolean;
  }
  // The mail health readout (`fauna.bridges.mail_health` projected by the shared
  // machine; mail-deliverability.md § The mail health readout). `null` until the
  // first hydrate — no section while absent.
  interface MailHealthCheckView {
    label_key: string;
    state: string;
    detail: string;
  }
  interface MailHealthView {
    state: string;
    checks: MailHealthCheckView[];
    last_outbound_delivered_at: number | null;
    last_inbound_accepted_at: number | null;
    delist_url: string | null;
  }
  interface MailPolicySnapshot {
    mail_enabled: boolean;
    // `SetupStatusReply::mail_unavailable_here` — read, never re-derived.
    mail_unavailable_here: boolean;
    auto_enable_mail_for_new_users: boolean;
    spam: SpamPolicyView;
    auth: AuthPolicyView;
    submission: SubmissionPolicyView;
    imap: ImapPolicyView;
    outbound: OutboundPolicyView;
    alias: AliasPolicyView;
    dmarc: DmarcPolicyView;
    microsoft_registration: MicrosoftRegistrationView;
    status: string; // "Idle" | "Loading" | "Working"
    baseline_publish_result: BaselinePublishView | null;
    baseline_state: BaselineStateView | null;
    health: MailHealthView | null;
    error: string | null;
    // The confirm button a first press armed (`MailPolicyConfirm` variant name),
    // held by the shared machine — the page keeps no arm of its own.
    armed: MailConfirm | null;
    // The last secret rotation's outcome, worded by `secretRotationText`.
    secret_rotation: unknown | null;
  }
  type MailConfirm =
    | 'ResetWarmup'
    | 'RotateListUnsubscribeSecret'
    | 'RotateSrsSecret'
    | 'PublishSpamBaseline';

  let machine: Awaited<ReturnType<typeof mailPolicyMachine>> | null = null;
  let snap = $state<MailPolicySnapshot | null>(null);
  // The page-level `error-message` (MessageBanner): load errors + the snapshot's
  // last-action error (`fauna.protocol.malformed` on an out-of-order spam write).
  let error = $state('');

  // ── Editable form state (seeded from the snapshot on each render). Integer
  // fields are strings so partial/blank edits round-trip; a group's save is live
  // only while every one of its numbers parses (`groupAnswer`). The mail-enable toggle
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
  // the Go MTA is pinned against the same tokens. Empty until the policy read's
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
  // DMARC reports group (the four edited fields)
  let dmarcRufPublish = $state(false);
  let dmarcFoMode = $state('');
  let dmarcAggregateRetention = $state('');
  let dmarcForensicRetention = $state('');
  let dmarcOutboundAggregate = $state(true);
  let dmarcFoOptions = $state<ReachPolicyOption[]>([]);
  // Microsoft registration for the Outlook import
  let msClientId = $state('');
  let msTenant = $state('');
  let msIsDefault = $state(true);

  const busy = $derived(snap?.status === 'Working' || snap?.status === 'Loading');

  /** The page's write answer — `MailPolicyMachine::write_possibility` over
   *  wasm (admin.md § Errors & edge cases, *Controls wait for their read*):
   *  still loading before the policy read, failed to load after a failed one,
   *  still saving while a change is on its way. It paints the mail-enable,
   *  new-user auto-enable and baseline-standing toggles, the rotate and
   *  publish confirms, and folds BEFORE every group's draft answer, so a save
   *  never sends defaults over a policy the page has not shown. */
  let writeAnswer = $state<Possibility | null>(null);
  const writeGate = $derived(formGate(writeAnswer));
  /** Whether the policy read ever resolved: until then a toggle's state is a
   *  guess and is not painted as `on`/`off`. */
  let policyRead = $state(false);

  /** The read that builds the machine: failed once its connect or first
   *  read threw, so every write control says failed to load, never nothing. */
  let machineBuild: ReadWord = 'pending';

  function repaint(): void {
    writeAnswer = machineAnswer(
      machine,
      (m) => m.writePossibility() as Possibility,
      machineBuild,
      readPossibility,
    );
  }

  /** Ask the write answer again at a press; refused, its reason goes on
   *  `error-message` and nothing is sent. */
  function admitWrite(): boolean {
    repaint();
    return admit(writeAnswer);
  }

  /** Dispatch a policy action, painting still saving while it is on its
   *  way; the snapshot carries any error. */
  async function run(action: unknown): Promise<void> {
    const m = machine;
    if (!m) return;
    try {
      await dispatchPainted(() => m.dispatch(action), repaint);
    } catch {
      /* the snapshot carries the error */
    }
    applySnapshot();
  }

  // A group's save answer (admin.md § Errors & edge cases → *A form's submit
  // is live only for a draft the nest would accept*; value-formatting.md
  // § Mail-knob validation): the policy read first (unreadable before the
  // snapshot), then the shared `mailKnobsDraftPossibility` over the group's
  // `u32` drafts, `u64` drafts and one-number-per-line lists — tui's
  // `save_draft_possibility`. One answer paints the save and refuses at its
  // press; a blank or malformed number is never replaced by the stored value.
  type Group = 'spam' | 'auth' | 'submission' | 'imap' | 'outbound' | 'alias' | 'dmarc';
  function groupDrafts(group: Group): [string[], string[], string[]] {
    switch (group) {
      case 'spam':
        return [
          [
            junk, reject, greylistDelay, maxConnPerMin, maxMessageBytes, bayesianWeight,
            bayesianMinSamples, bayesianFullConfidenceSamples, trainingHistoryRetention,
            unlistedRecipientPenalty,
          ],
          [],
          [],
        ];
      case 'auth':
        return [[maxFailures, maxConnPerIp], [], []];
      case 'submission':
        return [[subMaxPerDay, subMaxRecipients], [], []];
      case 'imap':
        return [
          [imapIdleTimeout, imapTombstoneRetention, imapBodystructureCache, imapMessageCount],
          [imapStorageBytes],
          [],
        ];
      case 'outbound':
        return [[outPermfailTimeout, outDelayWarning, outNdrRateLimit], [], [outRetrySchedule]];
      case 'alias':
        return [[aliasExactMax], [], []];
      case 'dmarc':
        return [[dmarcAggregateRetention, dmarcForensicRetention], [], []];
    }
  }
  function groupAnswer(group: Group): Possibility | null {
    const [counts, countsU64, lists] = groupDrafts(group);
    return afterGate(writeAnswer, () => mailKnobsDraftPossibility(counts, countsU64, lists));
  }
  const spamAnswer = $derived(groupAnswer('spam'));
  const authAnswer = $derived(groupAnswer('auth'));
  const submissionAnswer = $derived(groupAnswer('submission'));
  const imapAnswer = $derived(groupAnswer('imap'));
  const outboundAnswer = $derived(groupAnswer('outbound'));
  const aliasAnswer = $derived(groupAnswer('alias'));
  const dmarcAnswer = $derived(groupAnswer('dmarc'));
  /** The Microsoft save: the write answer first, then the pair check the
   *  nest runs (`mailMicrosoftDraftPossibility`). */
  const microsoftAnswer = $derived(
    afterGate(writeAnswer, () => mailMicrosoftDraftPossibility(msClientId, msTenant)),
  );

  /** Refuse a press on the page's `error-message` with the painted reason;
   *  `true` when the save may go on. */
  function admit(answer: Possibility | null): boolean {
    const refusal = formRefusal(answer);
    if (refusal === null) return true;
    if (refusal) error = refusal;
    return false;
  }

  // Each integer knob parses through the shared `parse_count` /
  // `parse_count_u64` (wasm) — read only once the group's answer is possible,
  // so every number parses and nothing falls back to a stored value.
  // `knobU64` is the one knob (IMAP storage-bytes) whose range can exceed u32;
  // byte ceilings stay within Number.MAX_SAFE_INTEGER.
  function knob(text: string): number {
    return parseCount(text) as number;
  }

  function knobU64(text: string): number {
    return parseCountU64(text) as number;
  }

  /** A one-number-per-line list (blank lines dropped), each line through
   *  `parse_count_u64` — the shared `parse_knob_u64_lines` rule. */
  function knobLines(text: string): number[] {
    return text
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0)
      .map((l) => parseCountU64(l) as number);
  }

  // Newline-separated text ↔ string list (the list-field convention, per the i18n
  // "one … per line" subtitles + the existing dnsbl_servers field).
  function linesToList(text: string): string[] {
    return text
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0);
  }

  function applySnapshot(): void {
    if (!machine) return;
    repaint();
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
    const dm = snap.dmarc;
    dmarcRufPublish = dm.ruf_publish;
    dmarcFoMode = dm.fo_mode;
    dmarcAggregateRetention = String(dm.aggregate_retention_days);
    dmarcForensicRetention = String(dm.forensic_retention_days);
    dmarcOutboundAggregate = dm.outbound_aggregate_enabled;
    const ms = snap.microsoft_registration;
    msClientId = ms.client_id;
    msTenant = ms.tenant;
    msIsDefault = ms.is_default;
  }

  /** The policy read: build the machine (once) and hydrate it. Every write
   *  control is painted from the moment the wasm core is up — still loading
   *  while the machine is built and hydrated, failed to load after — never
   *  disabled with nothing beside it. */
  async function readPolicy(): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      repaint();
      fcrdnsOptions = fcrdnsModeOptions();
      dmarcFoOptions = dmarcForensicModeOptions();
      imapDeleteOptions = imapDeleteNonemptyOptions();
      if (!machine) machine = await mailPolicyMachine(id.secretHex);
      repaint();
      await machine.hydrate();
      policyRead = true;
      applySnapshot();
    } catch (e) {
      if (!machine) machineBuild = 'failed';
      repaint();
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Read on every entry (a `goto()` of this route does not remount it) and,
  // while the policy is unread, again when the nest comes back.
  readOnEntryAndReconnect(readPolicy, async () => {
    if (!policyRead) await readPolicy();
  });

  // Full-PUT the whole Spam sub-struct (asked only once `spamAnswer` is
  // possible); the persisted snapshot supplies only the field with no control.
  function buildSpam(): SpamPolicyView {
    const base = snap!.spam;
    return {
      max_score_before_spam_folder: knob(junk),
      max_score_before_reject: knob(reject),
      dnsbl_servers: dnsbl
        .split('\n')
        .map((l) => l.trim())
        .filter((l) => l.length > 0),
      reject_no_rdns: rejectNoRdns,
      greylist_enabled: greylistEnabled,
      greylist_delay_secs: knob(greylistDelay),
      max_conn_per_min: knob(maxConnPerMin),
      fcrdns_mode: fcrdnsMode,
      helo_identity_required: heloIdentityRequired,
      reject_fcrdns_fail: rejectFcrdnsFail,
      max_message_bytes: knob(maxMessageBytes),
      bayesian_weight_milli: knob(bayesianWeight),
      bayesian_min_samples: knob(bayesianMinSamples),
      bayesian_full_confidence_samples: knob(bayesianFullConfidenceSamples),
      training_history_retention_days: knob(trainingHistoryRetention),
      unlisted_recipient_penalty: knob(unlistedRecipientPenalty),
      // Not rendered yet: carried through so a save never turns the standing
      // baseline publish off (which withdraws the deployment's baseline).
      baseline_standing_publish: base.baseline_standing_publish,
    };
  }

  function buildAuth(): AuthPolicyView {
    return {
      enforce_dmarc: enforceDmarc,
      enforce_dmarc_quarantine: enforceDmarcQuarantine,
      enforce_spf_hardfail: enforceSpfHardfail,
      enforce_dkim: enforceDkim,
      log_only: logOnly,
      max_auth_failures_per_minute: knob(maxFailures),
      max_conn_per_ip: knob(maxConnPerIp),
    };
  }

  // A box that cannot enable mail keeps the master toggle disabled; one where
  // mail is already on stays live so it can still be turned off.
  /** The enable toggle's shared answer (`enable_toggle_possibility`;
   *  `admin.md` § Errors & edge cases): blocked by the unavailable notice only
   *  for a turn-on on a box that cannot enable mail. */
  const availabilityGate = $derived(
    formGate(
      askPossibility(() =>
        mailEnableTogglePossibility(mailEnabled, Boolean(snap?.mail_unavailable_here)),
      ),
    ),
  );
  const unavailable = $derived(policyRead && availabilityGate.disabled);
  /** The toggle's whole answer: the write answer first (a page still loading
   *  says so first), then the availability answer above. */
  const enableAnswer = $derived(
    afterGate(writeAnswer, () =>
      mailEnableTogglePossibility(mailEnabled, Boolean(snap?.mail_unavailable_here)),
    ),
  );
  const enableGate = $derived(formGate(enableAnswer));

  async function handleToggleEnabled(e: Event): Promise<void> {
    const input = e.currentTarget as HTMLInputElement;
    const enabled = input.checked;
    // The press asks the painted answer again — the write answer, then for a
    // turn-on the availability answer; refused, it says why on
    // `error-message` and puts the switch back.
    repaint();
    const refusal = formRefusal(enabled ? enableAnswer : writeAnswer);
    if (refusal !== null) {
      if (refusal) error = refusal;
      input.checked = !enabled;
      return;
    }
    await run({ SetMailEnabled: { enabled } });
  }

  async function handleToggleAutoEnableNewUsers(e: Event): Promise<void> {
    const input = e.currentTarget as HTMLInputElement;
    const enabled = input.checked;
    if (!admitWrite()) {
      input.checked = !enabled;
      return;
    }
    await run({ SetAutoEnableMailForNewUsers: { enabled } });
  }

  async function handleRecheckHealth(): Promise<void> {
    if (!machine) return;
    await run('RecheckHealth');
  }

  // The same-button two-click confirms (warm-up reset, the two secret rotations,
  // the baseline publish) are one shared `Press`: the machine arms on a first
  // press and acts on a second press of the same button, and the snapshot's
  // `armed` relabels the button.
  //
  // The rotate and publish confirms write what the policy read decides, so
  // their press asks the write answer first (admin.md § Errors & edge cases,
  // *Controls wait for their read*); the warm-up reset reads the health
  // readout instead and keeps its own gate.
  async function press(button: MailConfirm): Promise<void> {
    if (button !== 'ResetWarmup' && !admitWrite()) return;
    await run({ Press: { button } });
  }

  function openDelist(url: string): void {
    window.open(url, '_blank', 'noopener,noreferrer');
  }

  async function handleSaveSpam(): Promise<void> {
    repaint();
    if (!admit(spamAnswer) || !machine || !snap) return;
    await run({ SaveSpam: { policy: buildSpam() } });
  }

  async function handleSaveAuth(): Promise<void> {
    repaint();
    if (!admit(authAnswer) || !machine || !snap) return;
    await run({ SaveAuth: { policy: buildAuth() } });
  }

  function buildSubmission(): SubmissionPolicyView {
    return {
      max_per_day: knob(subMaxPerDay),
      max_recipients_per_message: knob(subMaxRecipients),
    };
  }

  function buildImap(): ImapPolicyView {
    return {
      idle_timeout_secs: knob(imapIdleTimeout),
      tombstone_retention_days: knob(imapTombstoneRetention),
      delete_nonempty: imapDeleteNonempty,
      bodystructure_cache_max: knob(imapBodystructureCache),
      storage_bytes_default: knobU64(imapStorageBytes),
      message_count_default: knob(imapMessageCount),
    };
  }

  function buildOutbound(): OutboundPolicyView {
    return {
      retry_schedule_seconds: knobLines(outRetrySchedule),
      permanent_failure_timeout_hours: knob(outPermfailTimeout),
      delay_warning_at_hours: knob(outDelayWarning),
      ndr_rate_limit_days: knob(outNdrRateLimit),
      suppress_ndr_spf_hardfail: outSuppressNdrSpf,
      suppress_ndr_dmarc_reject: outSuppressNdrDmarc,
      postmaster_cc_bounces: outPostmasterCc, // read-only; echoes the persisted value
      tlsrpt_send_reports: outTlsrptSend,
      ipv6_enabled: outIpv6,
      treat_5xx_as_transient: linesToList(outTreat5xx),
    };
  }

  function buildAlias(): AliasPolicyView {
    return {
      exact_aliases_max: knob(aliasExactMax),
      reserved_local_parts: linesToList(aliasReserved),
      subaddressing_enabled: aliasSubaddressing,
      wildcard_prefix_enabled: aliasWildcardPrefix,
    };
  }

  async function handleSaveSubmission(): Promise<void> {
    repaint();
    if (!admit(submissionAnswer) || !machine || !snap) return;
    await run({ SaveSubmission: { policy: buildSubmission() } });
  }

  async function handleSaveImap(): Promise<void> {
    repaint();
    if (!admit(imapAnswer) || !machine || !snap) return;
    await run({ SaveImap: { policy: buildImap() } });
  }

  async function handleSaveOutbound(): Promise<void> {
    repaint();
    if (!admit(outboundAnswer) || !machine || !snap) return;
    await run({ SaveOutbound: { policy: buildOutbound() } });
  }

  // The four edited fields over the persisted catalog; a retention past its
  // ceiling is sent as typed — the nest refuses it onto `error-message`.
  function buildDmarc(): DmarcPolicyView {
    const base = snap!.dmarc;
    return {
      ...base,
      ruf_publish: dmarcRufPublish,
      fo_mode: dmarcFoMode,
      aggregate_retention_days: knob(dmarcAggregateRetention),
      forensic_retention_days: knob(dmarcForensicRetention),
      outbound_aggregate_enabled: dmarcOutboundAggregate,
    };
  }

  const forensicRetentionWarns = $derived.by(() => {
    const days = parseCount(dmarcForensicRetention.trim());
    return days != null && dmarcForensicRetentionWarns(days);
  });

  async function handleSaveDmarc(): Promise<void> {
    repaint();
    if (!admit(dmarcAnswer) || !machine || !snap) return;
    await run({ SaveDmarc: { policy: buildDmarc() } });
  }

  // The two drafts go out as typed once the shared pair check passes; a bad
  // pair is refused onto `error-message` at the press, before any round trip.
  async function handleSaveMicrosoft(): Promise<void> {
    repaint();
    if (!admit(microsoftAnswer) || !machine || !snap) return;
    await run({
        SetMicrosoftRegistration: { client_id: msClientId, tenant: msTenant },
      });
  }

  async function handleSaveAlias(): Promise<void> {
    repaint();
    if (!admit(aliasAnswer) || !machine || !snap) return;
    await run({ SaveAlias: { policy: buildAlias() } });
  }

  // Standing publish is its own gesture (`SetBaselineStandingPublish`, dispatched
  // immediately off the PERSISTED spam group), never a Spam-group draft.
  async function handleToggleBaselineStanding(e: Event): Promise<void> {
    const input = e.currentTarget as HTMLInputElement;
    if (!admitWrite() || !snap) {
      input.checked = !input.checked;
      return;
    }
    await run({
        SetBaselineStandingPublish: { enabled: !snap.spam.baseline_standing_publish },
      });
  }

  // The `admin-mail-spam-baseline-state` text: published / none, then the
  // waiting note when the last run was deferred. Blank until the read answers.
  function baselineStateText(st: BaselineStateView | null | undefined): string {
    if (!st) return '';
    let line =
      st.published && st.published_at_ms != null
        ? t.admin.mail_page.spam_baseline_state_published({
            contributors: String(st.contributors),
            date: localDate(st.published_at_ms),
          })
        : t.admin.mail_page.spam_baseline_state_none;
    if (st.deferred) line += ` ${t.admin.mail_page.spam_baseline_waiting}`;
    return line;
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
      {#if !unavailable}
        <span class="muted small">{t.admin.mail_page.enabled_subtitle}</span>
      {/if}
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_MAIL_ENABLED_TOGGLE}
      data-state={policyRead ? (mailEnabled ? 'on' : unavailable ? 'unavailable' : 'off') : undefined}
      checked={policyRead && mailEnabled}
      disabled={enableGate.disabled}
      title={enableGate.reason}
      data-reason={reasonAttr(enableGate)}
      onchange={handleToggleEnabled}
    />
  </label>
  {#if unavailable}
    <p class="muted small" data-testid={IDS.ADMIN_MAIL_UNAVAILABLE_NOTICE}>{t.mail_settings.unavailable_notice}</p>
  {/if}
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.mail_page.auto_enable_new_users_label}</span>
      <span class="muted small">{t.admin.mail_page.auto_enable_new_users_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_MAIL_AUTO_ENABLE_NEW_USERS_TOGGLE}
      checked={policyRead && autoEnableNewUsers}
      disabled={writeGate.disabled}
      title={writeGate.reason}
      data-reason={reasonAttr(writeGate)}
      onchange={handleToggleAutoEnableNewUsers}
    />
  </label>
</section>

<!-- ── Mail health readout (fauna.bridges.mail_health; read-only, a dumb render of
     the shared fold — mail-deliverability.md § The mail health readout) ── -->
{#if snap?.health}
  {@const health = snap.health}
  <section class="section" data-testid={IDS.ADMIN_MAIL_HEALTH_SECTION}>
    <h2>{t.admin.mail_page.health_title}</h2>
    <p class="small" data-testid={IDS.ADMIN_MAIL_HEALTH_STATUS}>
      {mailHealthStatusText(
        health.state,
        health.last_outbound_delivered_at,
        health.last_inbound_accepted_at,
      )}
    </p>
    {#each health.checks as check, i (i)}
      <div class="health-row" data-testid={IDS.ADMIN_MAIL_HEALTH_CHECK}>
        <span class="label-text" data-testid={IDS.ADMIN_MAIL_HEALTH_CHECK_LABEL}>
          {resolveLocalized({ key: check.label_key })}
        </span>
        <span class="small" data-testid={IDS.ADMIN_MAIL_HEALTH_CHECK_STATE}>
          {resolveLocalized(mailHealthCheckStateLabel(check.state))}
        </span>
        <span class="muted small" data-testid={IDS.ADMIN_MAIL_HEALTH_CHECK_DETAIL}>
          {#if i === 5}
            {mailHealthHeartbeatText(health.last_outbound_delivered_at)}
          {:else if i === 6}
            {mailHealthHeartbeatText(health.last_inbound_accepted_at)}
          {:else}
            {check.detail}
          {/if}
        </span>
      </div>
    {/each}
    {#if health.delist_url}
      {@const delistUrl = health.delist_url}
      <button
        class="btn"
        data-testid={IDS.ADMIN_MAIL_HEALTH_DELIST_LINK}
        data-href={delistUrl}
        onclick={() => openDelist(delistUrl)}
      >
        {t.admin.mail_page.health_delist}
      </button>
    {/if}
    <button
      class="btn"
      data-testid={IDS.ADMIN_MAIL_HEALTH_RECHECK_BUTTON}
      disabled={busy}
      onclick={handleRecheckHealth}
    >
      {t.admin.mail_page.health_recheck}
    </button>
    <button
      class="btn"
      data-testid={IDS.ADMIN_MAIL_HEALTH_WARMUP_RESET_BUTTON}
      disabled={busy}
      onclick={() => press('ResetWarmup')}
    >
      {snap?.armed === 'ResetWarmup' ? t.admin.mail_page.health_warmup_reset_confirm : t.admin.mail_page.health_warmup_reset}
    </button>
    <p class="muted small" data-testid={IDS.ADMIN_MAIL_HEALTH_SENDER_DASHBOARDS_NOTE}>
      {t.admin.mail_page.health_sender_dashboards_note}
    </p>
    {#each senderDashboardLinks() as link (link.key)}
      <button
        class="btn"
        data-testid={IDS.ADMIN_MAIL_HEALTH_SENDER_DASHBOARD_LINK}
        data-href={link.url}
        onclick={() => openDelist(link.url)}
      >
        {resolveLocalized({ key: link.label_key })}
      </button>
    {/each}
  </section>
{/if}

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
    disabled={busy || formGate(spamAnswer).disabled}
    title={formGate(spamAnswer).reason}
    data-reason={reasonAttr(formGate(spamAnswer))}
    onclick={handleSaveSpam}
  >
    {t.admin.mail_page.spam_save}
  </button>

  <!-- ── Deployment baseline (admin opt-in aggregate; publish_spam_baseline) ── -->
  <button
    class="btn"
    data-testid={IDS.ADMIN_MAIL_PUBLISH_SPAM_BASELINE_BUTTON}
    disabled={writeGate.disabled}
    title={writeGate.reason}
    data-reason={reasonAttr(writeGate)}
    onclick={() => press('PublishSpamBaseline')}
  >
    {snap?.armed === 'PublishSpamBaseline'
      ? t.admin.mail_page.publish_spam_baseline_confirm
      : t.admin.mail_page.publish_spam_baseline_button}
  </button>
  <p class="muted small">{t.admin.mail_page.publish_spam_baseline_subtitle}</p>
  <p class="muted small" data-testid={IDS.ADMIN_MAIL_PUBLISH_SPAM_BASELINE_RESULT}>
    {#if snap?.baseline_publish_result}
      {snap.baseline_publish_result.deferred
        ? t.admin.mail_page.spam_baseline_waiting
        : snap.baseline_publish_result.published
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
  <label class="switch-row">
    <span>
      <span class="label-text">{t.admin.mail_page.spam_baseline_standing_label}</span>
      <span class="muted small">{t.admin.mail_page.spam_baseline_standing_subtitle}</span>
    </span>
    <input
      type="checkbox"
      data-testid={IDS.ADMIN_MAIL_SPAM_BASELINE_STANDING_TOGGLE}
      checked={policyRead && (snap?.spam.baseline_standing_publish ?? false)}
      disabled={writeGate.disabled}
      title={writeGate.reason}
      data-reason={reasonAttr(writeGate)}
      onchange={handleToggleBaselineStanding}
    />
  </label>
  <p class="muted small" data-testid={IDS.ADMIN_MAIL_SPAM_BASELINE_STATE}>
    {baselineStateText(snap?.baseline_state)}
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
    disabled={busy || formGate(authAnswer).disabled}
    title={formGate(authAnswer).reason}
    data-reason={reasonAttr(formGate(authAnswer))}
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

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_SUBMISSION_SAVE_BUTTON} disabled={busy || formGate(submissionAnswer).disabled} title={formGate(submissionAnswer).reason} data-reason={reasonAttr(formGate(submissionAnswer))} onclick={handleSaveSubmission}>
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

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_IMAP_SAVE_BUTTON} disabled={busy || formGate(imapAnswer).disabled} title={formGate(imapAnswer).reason} data-reason={reasonAttr(formGate(imapAnswer))} onclick={handleSaveImap}>
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

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_OUTBOUND_SAVE_BUTTON} disabled={busy || formGate(outboundAnswer).disabled} title={formGate(outboundAnswer).reason} data-reason={reasonAttr(formGate(outboundAnswer))} onclick={handleSaveOutbound}>
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

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_ALIAS_SAVE_BUTTON} disabled={busy || formGate(aliasAnswer).disabled} title={formGate(aliasAnswer).reason} data-reason={reasonAttr(formGate(aliasAnswer))} onclick={handleSaveAlias}>
    {t.admin.mail_page.alias_save}
  </button>
</section>

<!-- ── DMARC reports (put_dmarc_policy; dual-read via get_dmarc_policy) ── -->
<section class="section">
  <h2>{t.admin.mail_page.dmarc_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.dmarc_group_desc}</p>

  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.dmarc_ruf_publish_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_DMARC_RUF_PUBLISH_TOGGLE} bind:checked={dmarcRufPublish} />
  </label>
  <p class="muted small" data-testid={IDS.ADMIN_MAIL_DMARC_RUF_WARNING}>{t.admin.mail_page.dmarc_ruf_warning}</p>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.dmarc_fo_mode_label}</span>
    <select class="input" data-testid={IDS.ADMIN_MAIL_DMARC_FO_MODE_SELECT} bind:value={dmarcFoMode}>
      {#each dmarcFoOptions as o (o.value)}
        <option value={o.value}>{resolveLocalized(o.label)}</option>
      {/each}
    </select>
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.dmarc_aggregate_retention_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_DMARC_AGGREGATE_RETENTION_INPUT} bind:value={dmarcAggregateRetention} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.dmarc_forensic_retention_label}</span>
    <input class="input" type="text" inputmode="numeric" data-testid={IDS.ADMIN_MAIL_DMARC_FORENSIC_RETENTION_INPUT} bind:value={dmarcForensicRetention} />
  </label>
  {#if forensicRetentionWarns}
    <p class="muted small">{t.admin.mail_page.dmarc_forensic_retention_warning}</p>
  {/if}
  <label class="switch-row">
    <span class="label-text">{t.admin.mail_page.dmarc_outbound_aggregate_label}</span>
    <input type="checkbox" data-testid={IDS.ADMIN_MAIL_DMARC_OUTBOUND_AGGREGATE_TOGGLE} bind:checked={dmarcOutboundAggregate} />
  </label>

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_DMARC_SAVE_BUTTON} disabled={busy || formGate(dmarcAnswer).disabled} title={formGate(dmarcAnswer).reason} data-reason={reasonAttr(formGate(dmarcAnswer))} onclick={handleSaveDmarc}>
    {t.admin.mail_page.dmarc_save}
  </button>
</section>

<!-- ── Microsoft registration for the Outlook import (set_microsoft_registration; read via fauna.setup.status) ── -->
<section class="section">
  <h2>{t.admin.mail_page.microsoft_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.microsoft_group_desc}</p>
  <p class="muted small">{t.admin.mail_page.microsoft_requirements}</p>
  <p class="muted small">{msIsDefault ? t.admin.mail_page.microsoft_using_default : t.admin.mail_page.microsoft_using_own}</p>

  <label class="field">
    <span class="label-text">{t.admin.mail_page.microsoft_client_id_label}</span>
    <input class="input" type="text" data-testid={IDS.ADMIN_MAIL_MICROSOFT_CLIENT_ID_INPUT} bind:value={msClientId} />
  </label>
  <label class="field">
    <span class="label-text">{t.admin.mail_page.microsoft_tenant_label}</span>
    <span class="muted small">{t.admin.mail_page.microsoft_tenant_subtitle}</span>
    <input class="input" type="text" data-testid={IDS.ADMIN_MAIL_MICROSOFT_TENANT_INPUT} bind:value={msTenant} />
  </label>

  <button class="btn primary" data-testid={IDS.ADMIN_MAIL_MICROSOFT_SAVE_BUTTON} disabled={busy || formGate(microsoftAnswer).disabled} title={formGate(microsoftAnswer).reason} data-reason={reasonAttr(formGate(microsoftAnswer))} onclick={handleSaveMicrosoft}>
    {t.admin.mail_page.microsoft_save}
  </button>
</section>

<!-- ── Secrets (rotate_list_unsubscribe_secret / rotate_srs_secret) ── -->
<section class="section">
  <h2>{t.admin.mail_page.secrets_group_title}</h2>
  <p class="muted small">{t.admin.mail_page.secrets_group_desc}</p>

  <button
    class="btn"
    data-testid={IDS.ADMIN_MAIL_LIST_UNSUBSCRIBE_SECRET_ROTATE_BUTTON}
    disabled={writeGate.disabled}
    title={writeGate.reason}
    data-reason={reasonAttr(writeGate)}
    onclick={() => press('RotateListUnsubscribeSecret')}
  >
    {snap?.armed === 'RotateListUnsubscribeSecret'
      ? t.admin.mail_page.list_unsubscribe_secret_rotate_confirm
      : t.admin.mail_page.list_unsubscribe_secret_rotate}
  </button>
  <button
    class="btn"
    data-testid={IDS.ADMIN_MAIL_FORWARD_SRS_SECRET_ROTATE_BUTTON}
    disabled={writeGate.disabled}
    title={writeGate.reason}
    data-reason={reasonAttr(writeGate)}
    onclick={() => press('RotateSrsSecret')}
  >
    {snap?.armed === 'RotateSrsSecret'
      ? t.admin.mail_page.forward_srs_secret_rotate_confirm
      : t.admin.mail_page.forward_srs_secret_rotate}
  </button>
  {#if snap?.secret_rotation}
    <p class="muted small">{resolveLocalized(secretRotationText(snap.secret_rotation))}</p>
  {/if}
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
  .health-row {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 1fr) minmax(0, 3fr);
    gap: 0.75rem;
    margin: 0.5rem 0;
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
