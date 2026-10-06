<script lang="ts">
  import { onMount } from 'svelte';
  import { identity } from '$lib/store';
  import {
    familyStatus,
    familyPolicyUpdate,
    familyApprovalsList,
    familyAllowBlockedDmPeer,
    familyApprovalsDecide,
    familyContactAdd,
    familyGraduate,
    familyTransfer,
    familyTransferAccept,
    familyTransferDecline,
    familyTransferCancel,
    familyDeviceMark,
    type FamilyStatus,
    type FamilyWardInfo,
    type FamilyIncomingTransferInfo,
    type FamilyApprovalEntry,
    type ReachPolicy,
  } from '$lib/rpc';
  import {
    ensureWasm,
    unknownSenderOptions,
    feedSourcesOptions,
    unknownPeerDmOptions,
    unknownSenderLabelRaw,
    feedSourcesLabelRaw,
    unknownPeerDmLabelRaw,
    contentFloorOptions,
    contentFloorLabelRaw,
    reachPolicySummaryFull,
    contentNoticeLine,
    usageTodayLine,
    approvalDisplayTextRaw,
    parseTimeOfDay,
    formatTimeOfDay,
    parseDailyMinutes,
    ageBandLine,
  } from '$lib/wasm';
  import { setWardScreenTime } from '$lib/screenTime.svelte';
  import { setGuardianHalf } from '$lib/contentPolicy.svelte';
  import { resolveLocalized, resolveLocalizedNested } from '$lib/i18n/localized';
  import type { FamilyAgeBandInfo } from '$lib/rpc';
  import { toBytes, actorHex, actorIdFromHex } from '$lib/hex';
  import { approvalText } from '$lib/family-approvals';
  import { blockedPeerRows, type BlockedPeer } from '$lib/ward-asks';
  import { setWardAsksFromStatus } from '$lib/wardAsks.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The `family` page (family-safety.md § App surface) — role-dependent
  // sections off ONE `fauna.family.status` read: the guardian section (wards +
  // the ONE shared reach-policy editor for the selected ward + the approvals
  // queue across ALL wards + contact-add + graduate) and the supervised section
  // (guardian handle + a read-only policy summary). Lifts the windows lead
  // (FamilyViewModel / FamilyPage.xaml — the reference client, priority #3);
  // shaped like the admin-users hub (sections of rows, per-row selects, a
  // page-level actionError). All calls go through the shared `FamilyClient` via
  // $lib/rpc's wasm seam — no composition in the page.
  //
  // ui.yaml's `family:` block carries exactly one (non-indexed) policy-editor
  // element set, so a guardian with multiple wards edits one at a time:
  // clicking a `family-ward-item` row loads that ward's policy into the shared
  // editor. The approvals queue is NOT ward-scoped — `fauna.family.approvals.list`
  // returns every ward's queue in one read.

  let status = $state<FamilyStatus | null>(null);
  let approvals = $state<FamilyApprovalEntry[]>([]);
  let actionError = $state('');
  let loading = $state(true);

  // The ward currently loaded into the shared policy editor (null until a
  // selection; `refresh()` re-selects the previous ward, else the first one).
  let selectedWard = $state<FamilyWardInfo | null>(null);

  // The four reach knobs, as EDITOR state. The two selects hold the LOCALIZED
  // label, not the wire value: the e2e select/read contract on web goes through
  // the <option value> attribute (and a <select>'s "text" is its `.value`), and
  // every app drives these pickers with the same localized string. The page
  // maps label ↔ wire value on load/save — see `unknownSenderWire` below.
  let policyContactApproval = $state(false);
  let policyUnknownSenderLabel = $state<string>(t.family.value_allow);
  let policyFederationContact = $state(true);
  let policyFeedSourcesLabel = $state<string>(t.family.value_allow);
  let policyUnknownPeerDmLabel = $state<string>(t.family.value_allow);
  /** Whether the guardian has touched `unknown_peer_dm`'s `<select>` since the
   *  editor last loaded a ward. Unlike every sibling reach knob, an absent
   *  `unknown_peer_dm` means "leave unchanged" (family-safety.md § The
   *  bridge-DM gate), so `savePolicy` must send it only on a genuine edit —
   *  else an unrelated save would echo back whatever this render happened to
   *  show, silently overwriting a value only a newer nest could have written.
   *  Set only by the `<select>`'s own `onchange` (a native DOM 'change' event
   *  a programmatic `loadPolicyFromWard` assignment never dispatches — no
   *  GTK-style signal guard needed here). */
  let policyUnknownPeerDmEdited = $state(false);

  // The v1.x content-policy pillar (family-safety.md § Content policy) — a
  // per-category guardian floor + the Notify toggle, edited in the same shared
  // editor. Same label-not-wire <select> contract as the two reach knobs above.
  // Default `inherit` = the unsupervised-equivalent (the ward's own thresholds
  // decide); default Notify off.
  let policyContentNsfwLabel = $state<string>(t.family.value_inherit);
  let policyContentSpamLabel = $state<string>(t.family.value_inherit);
  let policyContentPhishingLabel = $state<string>(t.family.value_inherit);
  let policyContentCommercialLabel = $state<string>(t.family.value_inherit);
  let policyContentNotify = $state(false);

  let contactAddInput = $state('');
  // Transfer initiation (family-safety.md § Graduation & transfer) — the same
  // hex-actor-id input convention as contact-add; the pending proposal renders
  // from the selected ward's `pending_transfer` (nest-confirmed, never local).
  let transferInput = $state('');
  // Graduation is reveal-then-confirm (the established cross-app shape):
  // `family-graduate-confirm-button` is not in the DOM until the reveal.
  let isConfirmingGraduate = $state(false);

  /** A band readout's text (the shared `ageBandLine`: band + how it was
   *  established), or '' when there is no nameable band — the caller renders
   *  nothing then (absent, never placeholdered; family-safety.md § App surface
   *  → *Age-band surfaces*). */
  function ageBandText(info: FamilyAgeBandInfo | null | undefined, own: boolean): string {
    if (!info) return '';
    return resolveLocalizedNested(ageBandLine(info.band, info.provenance, own));
  }

  function secret(): string | null {
    return $identity?.secretHex ?? null;
  }

  // ── Wire value ↔ localized label (the two <select>s) ────────────────────
  //
  // The option *set*, the label keys, and the fail-closed rule are shared
  // Rust (`fauna_core::format::{unknown_sender_options,feed_sources_options,
  // unknown_sender_label,feed_sources_label}` over wasm — same source
  // windows/macos/ios/android consume over UniFFI); this page owns only the
  // label-string ↔ wire-value bridge its <select>s need, since a web
  // <select>'s cross-app contract passes the LOCALIZED LABEL as the
  // <option value> (unlike the native pickers, which select by wire value —
  // family-safety.md § Where logic lives).
  //
  // A value this client cannot parse renders FAIL-CLOSED -- as the strictest
  // option, never the permissive one. Evolution is additive-everywhere and a
  // client may be OLDER than its nest (family-safety.md § The mail gate: "an
  // unrecognized knob value fails closed to `hold` ... `allow` would silently
  // void the guardian's policy"). Rendering an unknown value as "Allow" would
  // show the guardian a policy weaker than the one actually enforced -- and
  // saving would then write that `allow` back, downgrading the ward for real.
  function unknownSenderLabel(wire: string): string {
    return resolveLocalized(unknownSenderLabelRaw(wire));
  }
  function unknownSenderWire(label: string): string {
    return unknownSenderOptions().find((o) => resolveLocalized(o.label) === label)?.value ?? 'hold';
  }
  function feedSourcesLabel(wire: string): string {
    return resolveLocalized(feedSourcesLabelRaw(wire));
  }
  function feedSourcesWire(label: string): string {
    return feedSourcesOptions().find((o) => resolveLocalized(o.label) === label)?.value ?? 'block';
  }
  // `unknown_peer_dm`'s render direction is the mirror image of its siblings:
  // an ABSENT value is the `allow` DEFAULT, never the fail-closed `hold` — the
  // nest omits a knob sitting at its default (family-safety.md § The bridge-DM
  // gate). The save direction stays the ordinary fail-closed rule, since a
  // SEND always carries a real selection.
  function unknownPeerDmLabel(wire: string): string {
    return resolveLocalized(unknownPeerDmLabelRaw(wire));
  }
  function unknownPeerDmWire(label: string): string {
    return unknownPeerDmOptions().find((o) => resolveLocalized(o.label) === label)?.value ?? 'hold';
  }
  // The four content-floor selects share the same label↔wire bridge — the
  // catalog + fail-closed rule are shared Rust (`content_floor_options` /
  // `content_floor_label`); an unrecognized value fails closed to `block`.
  function contentFloorLabel(wire: string): string {
    return resolveLocalized(contentFloorLabelRaw(wire));
  }
  function contentFloorWire(label: string): string {
    return contentFloorOptions().find((o) => resolveLocalized(o.label) === label)?.value ?? 'block';
  }

  // The <select>s' option lists, in the ratified catalog order — resolved once
  // wasm is ready (see onMount) rather than at module-eval time.
  // Screen time (§ Screen time, Slice E) — the guardian's three inputs, held as
  // the raw typed strings so an empty field keeps meaning "this control is
  // unset". They are parsed through shared Rust at save, never here.
  let policyScreenWindowStart = $state('');
  let policyScreenWindowEnd = $state('');
  let policyScreenDailyMinutes = $state('');

  let unknownSenderOptionLabels = $state<string[]>([]);
  let feedSourcesOptionLabels = $state<string[]>([]);
  let unknownPeerDmOptionLabels = $state<string[]>([]);
  let contentFloorOptionLabels = $state<string[]>([]);

  /** The supervised side's read-only `family-policy-summary` — the four reach
   *  knobs PLUS each non-inherit content floor and, when on, the Notify line, as
   *  "{label}: {value}" lines, over the shared `ReachPolicy::summary_lines()`
   *  (family-safety.md § Content policy — ward transparency). */
  /** `usageTodayMinutes` folds the ward's own screen-time readout into this same
   *  summary rather than claiming a new ui.yaml ID: the supervised section's
   *  element set is `family-guardian-handle` + `family-policy-summary`, and the
   *  usage figure IS a line of "the active policy, read-only". It resolves
   *  through the shared `usageTodayLine` — the very call the guardian's own
   *  per-ward readout makes — so the ward and their guardian can never be shown
   *  different numbers (§ Screen time transparency rule). `null`/absent renders
   *  nothing: no budget, no accounting. */
  function formatPolicySummary(p: ReachPolicy, usageTodayMinutes?: number | null): string {
    const lines = reachPolicySummaryFull(p);
    if (usageTodayMinutes !== undefined && usageTodayMinutes !== null) {
      lines.push(usageTodayLine(usageTodayMinutes, p.screen_time?.daily_minutes ?? undefined));
    }
    return lines
      .map((line) => `${resolveLocalized(line.label)}: ${resolveLocalized(line.value)}`)
      .join('\n');
  }

  /** The guardian's per-ward `family-ward-usage-today` readout — the day's
   *  cross-device screen-time total for one ward (§ Screen time). Same shared
   *  `usageTodayLine` as the ward's own summary above; this owns only the join,
   *  mirroring `wardNoticesText`. */
  function wardUsageText(w: FamilyWardInfo): string {
    const line = usageTodayLine(
      w.usage_today_minutes ?? 0,
      w.policy?.screen_time?.daily_minutes ?? undefined,
    );
    return `${resolveLocalized(line.label)}: ${resolveLocalized(line.value)}`;
  }

  /** The guardian's per-ward `family-ward-content-notices` readout — one
   *  "{category}: {count}" line per Guardian Notify count on the ward
   *  (family-safety.md § Guardian Notify: category + count only, never content).
   *  Category label + "N flagged today" count come from shared Rust via
   *  `contentNoticeLine`, mirroring `formatPolicySummary`. */
  function wardNoticesText(w: FamilyWardInfo): string {
    return (w.content_notices ?? [])
      .map((n) => {
        const line = contentNoticeLine(n.category, n.count);
        return `${resolveLocalized(line.label)}: ${resolveLocalized(line.value)}`;
      })
      .join('; ');
  }

  // The row's display text lives in $lib/family-approvals (the one shared
  // rendering rule, unit-tested there): `peer_address` for a `mail_hold`
  // (localized no-sender label when empty — a held null-path message),
  // `summary` for every other kind.

  function approvalKey(a: FamilyApprovalEntry): string {
    const item = a.kind === 'mail_hold' ? a.message_id : a.peer_actor_id;
    return `${a.kind}:${actorHex(a.supervised_actor_id)}:${actorHex(item)}`;
  }

  // ── Load ─────────────────────────────────────────────────────────────────
  function loadPolicyFromWard(w: FamilyWardInfo) {
    selectedWard = w;
    policyContactApproval = w.policy.contact_approval;
    policyUnknownSenderLabel = unknownSenderLabel(w.policy.unknown_sender_mail);
    policyFederationContact = w.policy.federation_contact;
    policyFeedSourcesLabel = feedSourcesLabel(w.policy.feed_sources);
    // Render direction's mirror image: absent is the `allow` DEFAULT, not the
    // fail-closed `hold` (see `unknownPeerDmLabel`'s doc comment).
    policyUnknownPeerDmLabel = unknownPeerDmLabel(w.policy.unknown_peer_dm ?? 'allow');
    policyUnknownPeerDmEdited = false;
    // An absent content_policy is the all-inherit unsupervised-equivalent default.
    const cp = w.policy.content_policy;
    policyContentNsfwLabel = contentFloorLabel(cp?.nsfw ?? 'inherit');
    policyContentSpamLabel = contentFloorLabel(cp?.spam ?? 'inherit');
    policyContentPhishingLabel = contentFloorLabel(cp?.phishing ?? 'inherit');
    policyContentCommercialLabel = contentFloorLabel(cp?.commercial ?? 'inherit');
    policyContentNotify = w.policy.content_notify ?? false;
    // Screen time: an absent pillar is the unsupervised-equivalent default —
    // every field empty, i.e. no limit. Stored minutes render back through the
    // same shared formatter the parse side inverts, so what the guardian sees
    // is exactly what they could retype.
    const st = w.policy.screen_time;
    policyScreenWindowStart =
      st?.window_start === undefined || st?.window_start === null
        ? ''
        : formatTimeOfDay(st.window_start);
    policyScreenWindowEnd =
      st?.window_end === undefined || st?.window_end === null ? '' : formatTimeOfDay(st.window_end);
    policyScreenDailyMinutes =
      st?.daily_minutes === undefined || st?.daily_minutes === null
        ? ''
        : String(st.daily_minutes);
    isConfirmingGraduate = false;
    transferInput = '';
  }

  /** One `fauna.family.status` read fills BOTH sections, then the (not
   *  ward-scoped) approvals queue. Re-selects the previously-selected ward if
   *  it's still present, else auto-selects the first. */
  async function refresh() {
    const s = secret();
    if (!s) return;
    status = await familyStatus(s);
    // This page's own status read is the freshest view of the ward's OWN policy
    // anywhere in the app, so it also refreshes every client-enforced input —
    // the global `screen-time-lock` (family-safety.md § Screen time), the
    // content floor and Guardian Notify — off the reply's gated `supervision`
    // fold, exactly as the root layout's read does; otherwise a guardian's edit
    // would not bind until a reconnect.
    setWardScreenTime(
      status.supervision.screen_time,
      status.supervision.supervised_by?.handle ?? null,
      status.usage_today_minutes,
    );
    setGuardianHalf(status.supervision.content_policy, status.supervision.content_notify);
    // …and the ward's own outstanding asks, which the contacts / profile /
    // bridges ask surfaces render from (gated on supervised_by).
    setWardAsksFromStatus(status);
    const wards = status.wards ?? [];
    const prev = selectedWard ? actorHex(selectedWard.actor_id) : null;
    const still = prev ? wards.find((w) => actorHex(w.actor_id) === prev) : undefined;
    if (still) loadPolicyFromWard(still);
    else if (wards.length > 0) loadPolicyFromWard(wards[0]);
    else {
      selectedWard = null;
      isConfirmingGraduate = false;
    }
    approvals = await familyApprovalsList(s);
  }

  onMount(async () => {
    try {
      await ensureWasm();
      unknownSenderOptionLabels = unknownSenderOptions().map((o) => resolveLocalized(o.label));
      feedSourcesOptionLabels = feedSourcesOptions().map((o) => resolveLocalized(o.label));
      unknownPeerDmOptionLabels = unknownPeerDmOptions().map((o) => resolveLocalized(o.label));
      contentFloorOptionLabels = contentFloorOptions().map((o) => resolveLocalized(o.label));
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  // ── Guardian actions ─────────────────────────────────────────────────────
  async function savePolicy() {
    const s = secret();
    if (!s || !selectedWard) return;
    actionError = '';
    try {
      // Screen time is parsed + cross-field validated through shared Rust
      // BEFORE the RPC, on the same rule the nest enforces at policy.update, so
      // an entry the policy cannot hold surfaces its reason locally instead of
      // as a generic transport failure. `parse*` throws the reason string.
      const screenTime = {
        window_start: parseTimeOfDay(policyScreenWindowStart),
        window_end: parseTimeOfDay(policyScreenWindowEnd),
        daily_minutes: parseDailyMinutes(policyScreenDailyMinutes),
      };
      await familyPolicyUpdate(s, toBytes(selectedWard.actor_id), {
        contact_approval: policyContactApproval,
        unknown_sender_mail: unknownSenderWire(policyUnknownSenderLabel),
        federation_contact: policyFederationContact,
        feed_sources: feedSourcesWire(policyFeedSourcesLabel),
        // Absent means "leave unchanged" — send only when the guardian
        // actually touched the select (see `policyUnknownPeerDmEdited`'s doc
        // comment); an always-send would silently rewrite the ward's knob on
        // every unrelated save.
        unknown_peer_dm: policyUnknownPeerDmEdited
          ? unknownPeerDmWire(policyUnknownPeerDmLabel)
          : undefined,
        content_policy: {
          nsfw: contentFloorWire(policyContentNsfwLabel),
          spam: contentFloorWire(policyContentSpamLabel),
          phishing: contentFloorWire(policyContentPhishingLabel),
          commercial: contentFloorWire(policyContentCommercialLabel),
        },
        content_notify: policyContentNotify,
        // Present, so an all-empty editor CLEARS every limit. Absent would mean
        // "leave the pillar unchanged" and could never remove one.
        screen_time: screenTime,
      });
      // Refetch rather than trust the optimistic edit — the nest is the authority.
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function decide(a: FamilyApprovalEntry, approve: boolean) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await familyApprovalsDecide(
        s,
        toBytes(a.supervised_actor_id),
        a.kind,
        toBytes(a.peer_actor_id),
        toBytes(a.message_id),
        a.bridge_id ?? '',
        a.operation ?? '',
        a.target ?? '',
        a.peer_address ?? '',
        approve,
      );
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  /** `fauna.family.contact.add` — v1 takes a hex actor id (handle resolution is a
   *  later UX polish, not part of this seam). */
  async function addContact() {
    const s = secret();
    if (!s || !selectedWard) return;
    const peer = actorIdFromHex(contactAddInput);
    if (!peer) {
      actionError = t.family.contact_add_invalid_actor_id;
      return;
    }
    actionError = '';
    try {
      await familyContactAdd(s, toBytes(selectedWard.actor_id), peer);
      contactAddInput = '';
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function confirmGraduate() {
    const s = secret();
    if (!s || !selectedWard) return;
    actionError = '';
    try {
      await familyGraduate(s, toBytes(selectedWard.actor_id));
      isConfirmingGraduate = false;
      selectedWard = null;
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  /** `fauna.family.transfer` — propose a new guardian for the selected ward
   *  (pending until the target accepts; § Graduation & transfer). Same hex
   *  actor-id convention as contact-add. */
  async function proposeTransfer() {
    const s = secret();
    if (!s || !selectedWard) return;
    const target = actorIdFromHex(transferInput);
    if (!target) {
      actionError = t.family.contact_add_invalid_actor_id;
      return;
    }
    actionError = '';
    try {
      await familyTransfer(s, toBytes(selectedWard.actor_id), target);
      transferInput = '';
      // Refetch — the pending marker renders from nest-confirmed status.
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function cancelTransfer() {
    const s = secret();
    if (!s || !selectedWard) return;
    actionError = '';
    try {
      await familyTransferCancel(s, toBytes(selectedWard.actor_id));
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  /** `fauna.family.device.mark` — set/clear the guardian-enrolled-device marker
   *  on one of the selected ward's devices (§ Full visibility for young
   *  children, Slice F). Unlike the reach-policy knobs this is NOT batched
   *  behind Save: `device.mark` is its own per-device RPC, so the flip lands
   *  immediately and the refetch re-renders every row from nest-confirmed
   *  state. The device is named by its own `device_id` (never a row index), so
   *  a concurrent refetch that reorders the list cannot mark the wrong device. */
  async function setDeviceMark(deviceId: string, marked: boolean) {
    const s = secret();
    if (!s || !selectedWard) return;
    actionError = '';
    try {
      await familyDeviceMark(s, toBytes(selectedWard.actor_id), deviceId, marked);
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
      // The nest refused (e.g. the link dropped under us): re-read so the
      // toggle snaps back to the truth rather than showing the failed intent.
      await refresh();
    }
  }

  /** `family-blocked-peer-allow-button` — flip a guardian's earlier DM denial
   *  back to allow (family-safety.md § The bridge-DM gate → *The un-deny
   *  surface*). Rides the shared un-deny (`familyAllowBlockedDmPeer`) —
   *  idempotent and not queue-scoped, which is what lets it work long after
   *  the hold row that prompted the deny is gone. `peer` is the clicked ROW's
   *  own `(bridge_id, peer_id)` (`blockedPeerRows`), so the button can never
   *  un-deny somebody else. */
  async function allowBlockedPeer(peer: BlockedPeer) {
    const s = secret();
    if (!s || !selectedWard) return;
    actionError = '';
    try {
      await familyAllowBlockedDmPeer(s, toBytes(selectedWard.actor_id), peer);
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  /** Accept/decline an incoming proposal naming THIS account as the ward's
   *  new guardian. Accept re-points the link (the ward then appears in this
   *  account's own Wards list on the refetch). */
  async function decideIncoming(it: FamilyIncomingTransferInfo, accept: boolean) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      if (accept) await familyTransferAccept(s, toBytes(it.supervised_actor_id));
      else await familyTransferDecline(s, toBytes(it.supervised_actor_id));
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  let wards = $derived(status?.wards ?? []);
  let supervisedBy = $derived(status?.supervised_by ?? null);
  let incomingTransfers = $derived(status?.incoming_transfers ?? []);
</script>

<svelte:head><title>{t.family.title}</title></svelte:head>

<h1 data-testid={IDS.PAGE_HEADING}>{t.family.title}</h1>
<h2 data-testid={IDS.FAMILY_HEADING}>{t.family.title}</h2>

{#if actionError}
  <p data-testid={IDS.ERROR_MESSAGE} class="error">{actionError}</p>
{/if}

{#if loading}
  <p class="muted">{t.common.loading}</p>
{:else}
  <!-- ── Incoming-transfer prompts (any user can be a proposed guardian —
       this section is what the widened family-tab gate exists to reach). -->
  {#if incomingTransfers.length > 0}
    <section class="section">
      <h3>{t.family.incoming_transfers_heading}</h3>
      <ul class="list">
        {#each incomingTransfers as it, i (actorHex(it.supervised_actor_id))}
          <li class="row" data-testid={IDS.FAMILY_INCOMING_TRANSFER_ITEM} data-index={i}>
            <span class="approval-text"
              >{t.family.incoming_transfer_text({
                guardian: it.guardian_handle,
                ward: it.supervised_handle,
              })}</span>
            <button
              class="btn"
              data-testid={IDS.FAMILY_INCOMING_TRANSFER_ACCEPT_BUTTON}
              data-index={i}
              onclick={() => decideIncoming(it, true)}
            >{t.family.incoming_transfer_accept_button}</button>
            <button
              class="btn danger"
              data-testid={IDS.FAMILY_INCOMING_TRANSFER_DECLINE_BUTTON}
              data-index={i}
              onclick={() => decideIncoming(it, false)}
            >{t.family.incoming_transfer_decline_button}</button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <!-- ── Supervised section (rendered when this account is supervised) ── -->
  {#if supervisedBy}
    <section class="section">
      <p data-testid={IDS.FAMILY_GUARDIAN_HANDLE} class="guardian"
        >{t.family.guardian_label({ guardian: supervisedBy.handle })}</p>
      <h3>{t.family.policy_summary_heading}</h3>
      <p data-testid={IDS.FAMILY_POLICY_SUMMARY} class="policy-summary"
        >{status?.policy ? formatPolicySummary(status.policy, status.usage_today_minutes) : ''}</p>
      {#if ageBandText(status?.age_band, true)}
        <p data-testid={IDS.FAMILY_AGE_BAND_SUMMARY} class="muted small"
          >{ageBandText(status?.age_band, true)}</p>
      {/if}
    </section>
  {/if}

  <!-- ── Guardian section (rendered when the caller guards ≥1 account) ── -->
  {#if wards.length > 0}
    <section class="section">
      <h3>{t.family.wards_heading}</h3>
      <ul class="list">
        {#each wards as w, i (actorHex(w.actor_id))}
          <li>
            <button
              class="row ward"
              class:selected={selectedWard && actorHex(selectedWard.actor_id) === actorHex(w.actor_id)}
              data-testid={IDS.FAMILY_WARD_ITEM}
              data-index={i}
              onclick={() => loadPolicyFromWard(w)}
            >
              <span data-testid={IDS.FAMILY_WARD_HANDLE} data-index={i}>{w.handle}</span>
              {#if ageBandText(w.age_band, false)}
                <span class="muted small" data-testid={IDS.FAMILY_WARD_AGE_BAND} data-index={i}
                  >{ageBandText(w.age_band, false)}</span>
              {/if}
              {#if (w.content_notices ?? []).length > 0}
                <span
                  class="muted small"
                  data-testid={IDS.FAMILY_WARD_CONTENT_NOTICES}
                  data-index={i}>{wardNoticesText(w)}</span>
              {/if}
              <!-- Rendered only when the nest accounted a figure, which it does
                   only while a daily budget is set (§ Screen time — no usage
                   accounting without a declared policy). -->
              {#if w.usage_today_minutes !== undefined && w.usage_today_minutes !== null}
                <span
                  class="muted small"
                  data-testid={IDS.FAMILY_WARD_USAGE_TODAY}
                  data-index={i}>{wardUsageText(w)}</span>
              {/if}
            </button>
          </li>
        {/each}
      </ul>

      <!-- The ONE reach-policy editor, for the currently selected ward. -->
      {#if selectedWard}
        <div class="editor">
          <p class="muted small">{selectedWard.handle}</p>

          <!-- Toggles expose their state on `data-state` (the uniform
               get_attr(id, "state") toggle-read idiom) and flip on click. -->
          <button
            class="btn toggle"
            class:active={policyContactApproval}
            data-testid={IDS.FAMILY_POLICY_CONTACT_APPROVAL_TOGGLE}
            data-state={policyContactApproval ? 'on' : 'off'}
            onclick={() => (policyContactApproval = !policyContactApproval)}
          >{t.family.policy_contact_approval_label}</button>

          <label class="field">
            <span>{t.family.policy_unknown_sender_label}</span>
            <!-- The <option value> IS the localized label: the cross-app
                 select contract passes the label every app displays. -->
            <select data-testid={IDS.FAMILY_POLICY_UNKNOWN_SENDER_SELECT} bind:value={policyUnknownSenderLabel}>
              {#each unknownSenderOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>

          <button
            class="btn toggle"
            class:active={policyFederationContact}
            data-testid={IDS.FAMILY_POLICY_FEDERATION_TOGGLE}
            data-state={policyFederationContact ? 'on' : 'off'}
            onclick={() => (policyFederationContact = !policyFederationContact)}
          >{t.family.policy_federation_label}</button>

          <label class="field">
            <span>{t.family.policy_feed_sources_label}</span>
            <select data-testid={IDS.FAMILY_POLICY_FEED_SOURCES_SELECT} bind:value={policyFeedSourcesLabel}>
              {#each feedSourcesOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>
          <!-- `feed_sources` only gates NEW connections; inbound DMs riding an
               already-connected bridge account are the separate
               `unknown_peer_dm` knob below (family-safety.md § The bridge-DM
               gate). The caption names it by its own label. -->
          <p class="muted small caveat">{t.family.policy_feed_sources_caveat}</p>

          <!-- The bridge-DM gate (family-safety.md § The bridge-DM gate): whether
               a DM arriving over an already-connected bridge account, from a
               peer the ward has never messaged, is held for guardian review.
               `unknown_peer_dm` is `Option<String>` on the wire — absent means
               "leave unchanged" — so `savePolicy` gates the send on
               `policyUnknownPeerDmEdited`, armed only by a genuine user edit. -->
          <label class="field">
            <span>{t.family.policy_unknown_peer_dm_label}</span>
            <select
              data-testid={IDS.FAMILY_POLICY_UNKNOWN_PEER_DM_SELECT}
              bind:value={policyUnknownPeerDmLabel}
              onchange={() => (policyUnknownPeerDmEdited = true)}
            >
              {#each unknownPeerDmOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>

          <!-- ── Content policy (v1.x pillar 2, family-safety.md § Content
               policy) — a per-category floor: inherit | collapse | block. The
               same label-not-wire <select> contract as the reach knobs. -->
          <label class="field">
            <span>{t.family.policy_content_nsfw_label}</span>
            <select data-testid={IDS.FAMILY_POLICY_CONTENT_NSFW_SELECT} bind:value={policyContentNsfwLabel}>
              {#each contentFloorOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>
          <label class="field">
            <span>{t.family.policy_content_spam_label}</span>
            <select data-testid={IDS.FAMILY_POLICY_CONTENT_SPAM_SELECT} bind:value={policyContentSpamLabel}>
              {#each contentFloorOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>
          <label class="field">
            <span>{t.family.policy_content_phishing_label}</span>
            <select data-testid={IDS.FAMILY_POLICY_CONTENT_PHISHING_SELECT} bind:value={policyContentPhishingLabel}>
              {#each contentFloorOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>
          <label class="field">
            <span>{t.family.policy_content_commercial_label}</span>
            <select data-testid={IDS.FAMILY_POLICY_CONTENT_COMMERCIAL_SELECT} bind:value={policyContentCommercialLabel}>
              {#each contentFloorOptionLabels as label (label)}
                <option value={label}>{label}</option>
              {/each}
            </select>
          </label>

          <!-- Guardian Notify (family-safety.md § Guardian Notify) — category +
               count only, never content; default off. -->
          <button
            class="btn toggle"
            class:active={policyContentNotify}
            data-testid={IDS.FAMILY_POLICY_CONTENT_NOTIFY_TOGGLE}
            data-state={policyContentNotify ? 'on' : 'off'}
            onclick={() => (policyContentNotify = !policyContentNotify)}
          >{t.family.policy_content_notify_label}</button>

          <!-- Screen time (family-safety.md § Screen time, Slice E). The usage
               WINDOW is the hours the ward may use the account (so "no device
               after 21:00" is 07:00–21:00), and it may wrap midnight. Text
               inputs, not time pickers, because an empty field must keep
               meaning "this control is unset" — that is how a guardian clears
               a limit. Parsing is shared Rust (parseTimeOfDay), never JS. -->
          <h4 class="group-heading">{t.family.policy_screen_heading}</h4>
          <label class="field">
            <span>{t.family.policy_screen_window_start_label}</span>
            <input
              type="text"
              data-testid={IDS.FAMILY_POLICY_SCREEN_WINDOW_START_INPUT}
              bind:value={policyScreenWindowStart}
            />
          </label>
          <label class="field">
            <span>{t.family.policy_screen_window_end_label}</span>
            <input
              type="text"
              data-testid={IDS.FAMILY_POLICY_SCREEN_WINDOW_END_INPUT}
              bind:value={policyScreenWindowEnd}
            />
          </label>
          <label class="field">
            <span>{t.family.policy_screen_daily_minutes_label}</span>
            <input
              type="text"
              data-testid={IDS.FAMILY_POLICY_SCREEN_DAILY_MINUTES_INPUT}
              bind:value={policyScreenDailyMinutes}
            />
          </label>
          <!-- § Screen time's conforming-client bound, stated on the surface
               that exposes the knob (the same rule the feed_sources caveat
               follows for pillar 1). -->
          <p class="caveat">{t.family.policy_screen_caveat}</p>

          <button class="btn primary" data-testid={IDS.FAMILY_POLICY_SAVE_BUTTON} onclick={savePolicy}
            >{t.family.policy_save_button}</button>

          <!-- The guardian-enrolled-device marker (family-safety.md § Full
               visibility for young children, Slice F) — one row per device of
               the SELECTED ward. It lives inside the per-ward editor, not on
               the ward rows, for the same reason the policy knobs do: the
               index space then belongs to exactly one ward, so
               `family-device-mark-toggle[i]` is unambiguous with several wards
               on the page. Every row carries its device's own label, so a test
               finds a row by device name (substring) rather than trusting the
               nest's device order. -->
          <div class="devices">
            <h3>{t.family.ward_devices_heading}</h3>
            <p class="muted small">{t.family.ward_devices_hint({ handle: selectedWard.handle })}</p>
            {#each selectedWard.devices ?? [] as dev, i (dev.device_id)}
              <div class="device-row" data-testid={IDS.FAMILY_DEVICE_MARK_ITEM} data-index={i}>
                <span class="device-label">{dev.label}</span>
                <button
                  class="btn toggle"
                  class:active={dev.guardian_marked}
                  data-testid={IDS.FAMILY_DEVICE_MARK_TOGGLE}
                  data-index={i}
                  aria-pressed={dev.guardian_marked ? 'true' : 'false'}
                  data-state={dev.guardian_marked ? 'on' : 'off'}
                  onclick={() => setDeviceMark(dev.device_id, !dev.guardian_marked)}
                >{t.family.device_mark_label}</button>
              </div>
            {:else}
              <p class="muted">{t.family.no_ward_devices}</p>
            {/each}
          </div>

          <!-- The guardian's DENIED bridge-DM peers and the one-click flip back
               (family-safety.md § The bridge-DM gate → The un-deny surface) —
               without it a deny was a one-way door in the UI. Inside the
               per-ward editor, like the device list, so the index space belongs
               to exactly one ward. Row text IS the peer id (the only name this
               nest has for an external bridge peer); each allow button carries
               ITS OWN row's (bridge_id, peer_id). Only block verdicts arrive
               (the nest filters), so there is no "allowed peers" roster. -->
          <div class="blocked-peers">
            <h3>{t.family.blocked_peers_heading}</h3>
            <p class="muted small">{t.family.blocked_peers_hint({ handle: selectedWard.handle })}</p>
            {#each blockedPeerRows(selectedWard.blocked_dm_peers) as row, i (JSON.stringify([row.peer.bridge_id, row.peer.peer_id]))}
              <div class="device-row" data-testid={IDS.FAMILY_BLOCKED_PEER_ITEM} data-index={i}>
                <span class="device-label">{row.text}</span>
                <button
                  class="btn"
                  data-testid={IDS.FAMILY_BLOCKED_PEER_ALLOW_BUTTON}
                  data-index={i}
                  onclick={() => allowBlockedPeer(row.peer)}
                >{t.family.blocked_peer_allow}</button>
              </div>
            {:else}
              <p class="muted">{t.family.no_blocked_peers}</p>
            {/each}
          </div>

          <!-- Pre-approve a contact for the selected ward. -->
          <div class="contact-add">
            <input
              class="input"
              data-testid={IDS.FAMILY_CONTACT_ADD_INPUT}
              placeholder={t.family.contact_add_placeholder}
              bind:value={contactAddInput}
            />
            <button class="btn" data-testid={IDS.FAMILY_CONTACT_ADD_BUTTON} onclick={addContact}
              >{t.family.contact_add_button}</button>
          </div>

          <!-- Transfer initiation (§ Graduation & transfer) — per selected
               ward, same hex-actor-id convention as contact-add. The pending
               marker renders from the ward's nest-confirmed `pending_transfer`
               and clears on accept/decline/cancel/expiry. -->
          <div class="transfer">
            {#if selectedWard.pending_transfer}
              <p class="pending" data-testid={IDS.FAMILY_TRANSFER_PENDING}
                >{t.family.transfer_pending({
                  handle: selectedWard.pending_transfer.proposed_guardian_handle,
                })}</p>
              <button class="btn" data-testid={IDS.FAMILY_TRANSFER_CANCEL_BUTTON} onclick={cancelTransfer}
                >{t.family.transfer_cancel_button}</button>
            {:else}
              <input
                class="input"
                data-testid={IDS.FAMILY_TRANSFER_INPUT}
                placeholder={t.family.transfer_placeholder}
                bind:value={transferInput}
              />
              <button class="btn" data-testid={IDS.FAMILY_TRANSFER_BUTTON} onclick={proposeTransfer}
                >{t.family.transfer_button}</button>
            {/if}
          </div>

          <!-- Graduation — reveal-then-confirm. -->
          <div class="graduate">
            <button
              class="btn"
              data-testid={IDS.FAMILY_GRADUATE_BUTTON}
              onclick={() => (isConfirmingGraduate = true)}
            >{t.family.graduate_button}</button>
            {#if isConfirmingGraduate}
              <button
                class="btn danger"
                data-testid={IDS.FAMILY_GRADUATE_CONFIRM_BUTTON}
                onclick={confirmGraduate}
              >{t.family.graduate_confirm_button({ handle: selectedWard.handle })}</button>
            {/if}
          </div>
        </div>
      {/if}

      <h3>{t.family.approvals_heading}</h3>
      {#if approvals.length === 0}
        <p class="muted">{t.family.no_approvals}</p>
      {:else}
        <ul class="list">
          {#each approvals as a, i (approvalKey(a))}
            <li class="row" data-testid={IDS.FAMILY_APPROVAL_ITEM} data-index={i}>
              <span class="approval-text">{approvalText(a, approvalDisplayTextRaw)}</span>
              <span class="muted small">{a.supervised_handle}</span>
              <button
                class="btn"
                data-testid={IDS.FAMILY_APPROVAL_APPROVE_BUTTON}
                data-index={i}
                onclick={() => decide(a, true)}>{t.family.approve}</button>
              <button
                class="btn danger"
                data-testid={IDS.FAMILY_APPROVAL_DENY_BUTTON}
                data-index={i}
                onclick={() => decide(a, false)}>{t.family.deny}</button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {:else if !supervisedBy}
    <p class="muted">{t.family.no_wards}</p>
  {/if}
{/if}

<style>
  h2 {
    margin-top: 0.25rem;
    font-size: 1rem;
    color: var(--text-muted, #8b949e);
  }
  h3 {
    margin-top: 1.25rem;
    font-size: 0.95rem;
  }
  .section {
    margin-bottom: 1.5rem;
    max-width: 40rem;
  }
  .error {
    color: var(--color-error, #f85149);
  }
  .muted {
    color: var(--text-muted, #8b949e);
  }
  .small {
    font-size: 0.8rem;
  }
  .guardian {
    font-weight: 600;
  }
  .policy-summary {
    white-space: pre-line;
    opacity: 0.85;
  }
  .list {
    list-style: none;
    padding: 0;
    margin: 0.5rem 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.375rem 0;
    flex-wrap: wrap;
  }
  .ward {
    width: 100%;
    text-align: left;
    padding: 0.5rem 0.625rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 4px;
    background: none;
    color: inherit;
    cursor: pointer;
  }
  .ward.selected {
    border-color: var(--accent, #3b82f6);
    background: var(--bg-hover, #1c2128);
  }
  .editor {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
    margin: 0.75rem 0;
    padding: 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .caveat {
    margin: 0;
    max-width: 34rem;
  }
  .toggle.active {
    border-color: var(--accent, #3b82f6);
    color: var(--accent, #3b82f6);
  }
  .contact-add {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .devices {
    align-self: stretch;
  }
  .device-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    padding: 0.25rem 0;
  }
  .device-label {
    flex: 1;
    min-width: 8rem;
  }
  .graduate {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .transfer {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    align-items: center;
  }
  .pending {
    margin: 0;
    font-style: italic;
  }
  .approval-text {
    flex: 1;
    min-width: 10rem;
  }
</style>
