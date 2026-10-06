<script lang="ts">
  import { onMount } from 'svelte';
  import { identity } from '$lib/store';
  import {
    adminUsersList,
    adminUsersListAll,
    adminUsersUpdate,
    adminUsersEvict,
    adminUsersSuspend,
    adminUsersCancelEviction,
    adminAdminsAdd,
    adminAdminsRemove,
    adminTiersList,
    adminInviteCodesList,
    adminInviteCodesCreate,
    adminInviteCodesDelete,
    adminInviteRequestsList,
    adminInviteRequestsApprove,
    adminInviteRequestsDeny,
    adminSetRegistrationMode,
    adminSetAgeVerificationRequired,
    adminUsersCreate,
    asRegistrationMode,
    setupStatus,
    type AdminUser,
    type AdminTier,
    type AdminInviteCode,
    type AdminInviteRequest,
    type RegistrationMode,
  } from '$lib/rpc';
  import { toBytes, actorHex, actorIdFromHex } from '$lib/hex';
  import { t } from '$lib/i18n/strings';
  import {
    mailServingStatusLabel,
    adminUserRowControls,
    adminPickerOption,
    registrationModeOptions,
    ageBandOptions,
    claimedAgeBandOption,
    ageClaimLabel,
    ageBandLabel,
    shortId,
    totalPages,
    currentPage,
    nextPageOffset,
    prevPageOffset,
    type RegistrationModeOption,
    type AgeBandOption,
  } from '$lib/wasm';
  import { resolveLocalized, resolveLocalizedNested } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  // Consolidated admin-users hub (admin.md § 2) — five sections: Pending
  // requests / Registration / Admit / Invite / Users. Drives the shared `fauna.admin.*`
  // WS-RPC kinds via the wasm `AdminClient` (no `/admin/api/*` HTTP twins).
  // The tier is the quota (admin.md § Users): admission and change-tier both
  // assign a tier; there is no separate per-user quota control. Registration is
  // the same mental model one level up — "may anyone let themselves on?".

  const PAGE_SIZE = 50;

  let tiers = $state<AdminTier[]>([]);
  let requests = $state<AdminInviteRequest[]>([]);
  let inviteCodes = $state<AdminInviteCode[]>([]);
  let users = $state<AdminUser[]>([]);
  let total = $state(0);
  let offset = $state(0);
  let actionError = $state('');
  let loading = $state(true);

  // Section 1 — pending requests: per-row chosen tier + guardian + deny reason.
  let requestTier = $state<Record<number, string>>({});
  let requestGuardianIdx = $state<Record<number, number>>({});
  // Per-row age band (an `ageBandOptions` VALUE), seeded from the applicant's
  // claim — see *The age-band pickers* below.
  let requestAgeBand = $state<Record<number, string>>({});
  let denyReasons = $state<Record<number, string>>({});
  // Section 2 — registration: the posture + the orthogonal free-tier ceiling,
  // saved together by one `fauna.admin.set_registration_mode` call.
  //
  // `registrationMode` is null exactly when this client cannot name the nest's
  // posture — a *newer* nest whose mode we
  // predate (`asRegistrationMode`). Both render read-only: offering a Save would
  // let us overwrite a real posture with a guess. `unknownMode` carries the raw
  // value for that message.
  let registrationMode = $state<RegistrationMode | null>(null);
  let unknownMode = $state<string | null>(null);
  // The picker's option catalog (wire value + localized label, in display
  // order) — the shared `fauna_client_admin::registration_mode_options`, so
  // this page stops hand-listing the three `<option>`s (admin.md § Where
  // logic lives).
  let registrationModeOptionsList = $state<RegistrationModeOption[]>([]);
  // The ceiling as typed: '' means blank, i.e. no cap.
  let maxFreeUsers = $state('');
  // The age require-knob: the draft the toggle flips, and the nest's persisted
  // value it was seeded from — the save sends `set_age_verification_required`
  // only when the two differ (admin.md § 2 → Registration: one gesture).
  let ageVerificationDraft = $state(false);
  let ageVerificationPersisted = $state(false);
  // Section 3 — admit: direct admission drafts (`public-mode.md` §
  // Registration & Identity). A blank handle admits the deliberate
  // handle-less state; there is no free-text label on this path.
  let admitActorInput = $state('');
  let admitHandleInput = $state('');
  let admitTier = $state('');
  // Section 4 — invite: create form.
  let showCreateForm = $state(false);
  let newCodeTier = $state('');
  let newCodeMaxUses = $state(1);
  let newCodeGuardianIdx = $state(0);
  let newCodeAgeBand = $state('');
  let mintedCode = $state('');
  // Section 5 — users: per-row chosen tier (keyed by actor-id hex).
  let userTier = $state<Record<string, string>>({});

  // ── The guardian pickers (family-safety.md § App surface — admission
  // surfaces): both admission paths (approve a pending request / mint an invite
  // code) may designate a guardian, making the admitted account SUPERVISED —
  // the link + its default policy are created in the same admission transaction.
  // Built like the admin-web apex picker (`admin-web-apex-actor-select`): option
  // 0 = "None" (an ordinary, unsupervised admission — the default), options 1..N
  // = every non-suspended account on the nest (`pickerUsers` — never the Users
  // page on screen, admin.md § 2 → *Which accounts a picker offers*). Each
  // option's `value` is `adminPickerOption(u)` —
  // the shared handle-or-full-hex decision (`fauna_client_admin::admin_picker_option`,
  // `admin.md` § 2 → *What identifies a user in an admin picker*: unique on the
  // nest, where the display label is freely editable and non-unique) — so no
  // client re-derives it (priority #1/#2). Onchange still dispatches by
  // `selectedIndex` against a parallel actor-id map, but the option `value`
  // itself must ALSO be unique: the automation bridge selects options via
  // Playwright's `select_option`, which matches the DOM `<option value>` —
  // with the old label-keyed value, two same-labelled users made that match
  // ambiguous (first-match wins, possibly the wrong account). A handle is
  // unique by construction, so this is injective both ways.
  let pickerUsers = $state<AdminUser[]>([]);
  let guardians = $derived.by(() => {
    const options: string[] = [t.admin.users_page.guardian_none];
    const ids: (Uint8Array | null)[] = [null];
    for (const u of pickerUsers) {
      if (u.suspended) continue;
      options.push(adminPickerOption(u));
      ids.push(toBytes(u.actor_id));
    }
    return { options, ids };
  });

  /** The chosen guardian's actor id, or `undefined` for "None" (option 0) — the
   *  `guardianActor` argument both admission calls take as optional. */
  function guardianActorAt(idx: number): Uint8Array | undefined {
    return guardians.ids[idx] ?? undefined;
  }

  /** The option a picker currently shows (its `value` == its label). Falls back
   *  to "None" when the previously-picked index is out of range after a refetch. */
  function guardianOptionAt(idx: number): string {
    return guardians.options[idx] ?? guardians.options[0];
  }

  // ── The age-band pickers (family-safety.md § App surface → *Age-band
  // surfaces*): both admission paths may set the supervised account's band
  // beside its guardian. The options are the shared `ageBandOptions` catalog
  // by VALUE (*not set* + the four bands), so this page never spells the band
  // vocabulary. A band presupposes a guardian: each select is enabled only
  // while its guardian picker names someone, is reset to *not set* when the
  // guardian is cleared, and the band rides the admission call only beside a
  // guardian (the nest refuses a band without one — that refusal stays the
  // authority, this gate is UX). The request row's select seeds from the
  // applicant's claim (`claimedAgeBandOption`).
  let ageBandOptionsList = $state<AgeBandOption[]>([]);
  // The *not set* value — the shared seed for "no claim", never a TS literal.
  let ageBandNotSet = $state('');

  /** The band to send beside `guardianIdx`'s guardian, or `undefined` with no
   *  guardian (an ordinary admission carries no band). */
  function ageBandFor(guardianIdx: number, band: string | undefined): string | undefined {
    return guardianActorAt(guardianIdx) ? band : undefined;
  }

  function secret(): string | null {
    return $identity?.secretHex ?? null;
  }

  function defaultTier(): string {
    return tiers[0]?.name ?? 'free';
  }

  async function refresh() {
    const s = secret();
    if (!s) return;
    tiers = await adminTiersList(s);
    requests = await adminInviteRequestsList(s);
    inviteCodes = await adminInviteCodesList(s);
    // Section 2 — seed the registration posture + ceiling from the nest.
    const status = await setupStatus(s);
    registrationMode = asRegistrationMode(status.registration_mode);
    unknownMode = registrationMode === null ? status.registration_mode : null;
    maxFreeUsers = status.max_free_users === null ? '' : String(status.max_free_users);
    ageVerificationPersisted = status.age_verification_required ?? false;
    ageVerificationDraft = ageVerificationPersisted;
    // The guardian pickers' source — every account, read before the page so a
    // rendered Users page never shows pickers still missing accounts.
    pickerUsers = await adminUsersListAll(s);
    const page = await adminUsersList(s, PAGE_SIZE, offset);
    users = page.users;
    total = page.total;
    // Seed per-row selects from current values.
    const nextRequestTier: Record<number, string> = {};
    const nextRequestGuardian: Record<number, number> = {};
    const nextRequestAgeBand: Record<number, string> = {};
    for (const r of requests) {
      nextRequestTier[r.id] = requestTier[r.id] ?? defaultTier();
      // Keep a guardian already picked on this row (0 = "None", the default).
      nextRequestGuardian[r.id] = requestGuardianIdx[r.id] ?? 0;
      // Keep a band already picked; else seed from the applicant's claim.
      nextRequestAgeBand[r.id] = requestAgeBand[r.id] ?? claimedAgeBandOption(r.age_band);
    }
    requestTier = nextRequestTier;
    requestGuardianIdx = nextRequestGuardian;
    requestAgeBand = nextRequestAgeBand;
    const nextUserTier: Record<string, string> = {};
    for (const u of users) nextUserTier[actorHex(u.actor_id)] = u.tier;
    userTier = nextUserTier;
  }

  onMount(async () => {
    registrationModeOptionsList = registrationModeOptions();
    ageBandOptionsList = ageBandOptions();
    ageBandNotSet = claimedAgeBandOption(null);
    newCodeAgeBand = ageBandNotSet;
    try {
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  });

  function onlyPending(rs: AdminInviteRequest[]): AdminInviteRequest[] {
    return rs.filter((r) => r.is_pending);
  }

  // ── Section 1 — Pending requests ─────────────────────────────────────────
  async function approve(req: AdminInviteRequest) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminInviteRequestsApprove(
        s,
        req.id,
        requestTier[req.id] ?? defaultTier(),
        guardianActorAt(requestGuardianIdx[req.id] ?? 0),
        ageBandFor(requestGuardianIdx[req.id] ?? 0, requestAgeBand[req.id]),
      );
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function deny(req: AdminInviteRequest) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminInviteRequestsDeny(s, req.id, denyReasons[req.id] ?? '');
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Section 2 — Registration ─────────────────────────────────────────────

  /** The typed ceiling as the wire value: blank ⇒ `undefined`, which *clears* the
   *  cap (mode + ceiling are one decision, always saved together). */
  function capToSave(): number | undefined {
    const trimmed = maxFreeUsers.trim();
    return trimmed === '' ? undefined : Number(trimmed);
  }

  /** Blank, or a non-negative integer. Guards the `<input type="text">` (blank has
   *  to stay expressible, so it cannot be `type="number"`) before `Number()` turns
   *  junk into `NaN` and BigInt() throws inside the wrapper. */
  function capIsValid(): boolean {
    const trimmed = maxFreeUsers.trim();
    return trimmed === '' || /^\d+$/.test(trimmed);
  }

  async function saveRegistration() {
    const s = secret();
    // A null mode means we could not name the nest's posture — the section is
    // read-only in that case and this handler is unreachable, but never write a
    // guess even if that changes.
    if (!s || registrationMode === null) return;
    actionError = '';
    if (!capIsValid()) {
      actionError = t.admin.users_page.max_free_users_hint;
      return;
    }
    try {
      await adminSetRegistrationMode(s, registrationMode, capToSave());
      // The require-knob rides the same save — a second kind, sent only when
      // the toggle's value changed (family-safety.md § App surface →
      // *Age-band surfaces*).
      if (ageVerificationDraft !== ageVerificationPersisted) {
        await adminSetAgeVerificationRequired(s, ageVerificationDraft);
      }
      // The re-read is the acknowledgement: the controls re-seed from the nest,
      // so what the admin sees after a save is the persisted posture, not their
      // typing. (admin.md § 2 specifies no separate confirmation element.)
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Section 3 — Admit (direct admission, public-mode.md § Registration &
  //    Identity) ──────────────────────────────────────────────────────────
  // Client-side 64-hex actor validation (mirrors tui's `admit_mutation` /
  // linux's click handler): a malformed id is a user error, never
  // dispatched — the nest would refuse it anyway, and failing local keeps
  // the message actionable. A blank handle ⇒ `undefined`, the deliberate
  // handle-less admission. Success re-render is the Users section's own
  // refetch (shared with tier-change/evict).
  async function admitUser() {
    const s = secret();
    if (!s) return;
    actionError = '';
    const raw = admitActorInput.trim();
    const actor = raw.length === 64 ? actorIdFromHex(raw) : null;
    if (!actor) {
      actionError = t.admin.users_page.admit_actor_hint;
      return;
    }
    const handle = admitHandleInput.trim() || undefined;
    try {
      await adminUsersCreate(s, actor, admitTier || defaultTier(), handle);
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Section 4 — Invite ───────────────────────────────────────────────────
  async function mintCode() {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      // Empty code ⇒ the nest mints + returns one (admin.md § 3, mint-on-empty).
      // A picked guardian makes the redeeming account supervised.
      // Clamp to >= 1 like the native apps: the input's min="1" is decorative
      // (typing "-5" still binds), and the nest stores `uses` unvalidated with
      // redemption checking `uses_left > 0` — a 0/negative mint is a born-dead
      // code. An emptied field binds null → 1.
      mintedCode = await adminInviteCodesCreate(
        s,
        newCodeTier || defaultTier(),
        Math.max(1, Math.round(newCodeMaxUses || 1)),
        guardianActorAt(newCodeGuardianIdx),
        ageBandFor(newCodeGuardianIdx, newCodeAgeBand),
      );
      inviteCodes = await adminInviteCodesList(s);
      showCreateForm = false;
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function deleteCode(code: string) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminInviteCodesDelete(s, code);
      inviteCodes = await adminInviteCodesList(s);
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Section 5 — Users ────────────────────────────────────────────────────
  async function changeTier(user: AdminUser) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminUsersUpdate(s, toBytes(user.actor_id), userTier[actorHex(user.actor_id)], user.label);
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function evict(user: AdminUser) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      // The evict handler *requires* a reason (unlike suspend, which defaults it),
      // so one is sent — from i18n, not a hard-coded English literal (priority #1;
      // linux sends the same `evict_default_reason` constant).
      await adminUsersEvict(
        s,
        toBytes(user.actor_id),
        t.admin.users_page.evict_default_reason,
        'other',
      );
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  // Cut the user off now, no delete timeline. Empty reason/category take the nest's
  // canonical defaults ("suspended by admin" / "other") rather than hard-coding them
  // client-side. Reversible from the restore control the row then offers.
  async function suspend(user: AdminUser) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminUsersSuspend(s, toBytes(user.actor_id), '', '');
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function cancelEvict(user: AdminUser) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminUsersCancelEviction(s, toBytes(user.actor_id));
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  // Grant/revoke the admin role (admin.md § Admin continuity and succession,
  // instrument 1). Both schedule a 24h-delayed pending action — a scheduled
  // reply (no error) IS success; the row does not flip to an admin row right
  // away, so the refresh below is for the rest of the page, not proof of the
  // grant itself.
  async function makeAdmin(user: AdminUser) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminAdminsAdd(s, toBytes(user.actor_id));
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function removeAdmin(user: AdminUser) {
    const s = secret();
    if (!s) return;
    actionError = '';
    try {
      await adminAdminsRemove(s, toBytes(user.actor_id));
      await refresh();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    }
  }

  async function nextPage() {
    const next = nextPageOffset(offset, total, PAGE_SIZE);
    if (next === undefined) return;
    offset = next;
    await refresh();
  }

  async function prevPage() {
    const prev = prevPageOffset(offset, PAGE_SIZE);
    if (prev === undefined) return;
    offset = prev;
    await refresh();
  }
</script>

<svelte:head><title>{t.common.admin} · {t.admin.users_page.title}</title></svelte:head>

<h1 data-testid={IDS.ADMIN_USERS_HEADING}>{t.admin.users_page.title}</h1>

{#if actionError}
  <p data-testid={IDS.ADMIN_USERS_ACTION_ERROR} class="error">{actionError}</p>
{/if}

{#if loading}
  <p class="muted">{t.common.loading}</p>
{:else}
  <!-- Section 1 — Pending requests -->
  <section data-testid={IDS.ADMIN_USERS_REQUESTS_SECTION} class="section">
    <h2>{t.admin.users_page.section_requests}</h2>
    {#if onlyPending(requests).length === 0}
      <p class="muted">{t.admin.invite_requests_page.empty}</p>
    {:else}
      <ul class="list" data-testid={IDS.ADMIN_INVITE_REQUESTS_LIST}>
        {#each onlyPending(requests) as req, i (req.id)}
          <li class="row" data-index={i}>
            <span class="actor" data-testid={IDS.INVITE_REQUEST_ROW_ACTOR} title={actorHex(req.actor_id)}
              >{shortId(actorHex(req.actor_id))}</span>
            <span class="handle" data-testid={IDS.INVITE_REQUEST_ROW_HANDLE}>{req.handle}</span>
            <span class="message" data-testid={IDS.INVITE_REQUEST_ROW_MESSAGE}>{req.message}</span>
            <select
              data-testid={IDS.INVITE_REQUEST_ROW_TIER_SELECT}
              data-index={i}
              bind:value={requestTier[req.id]}
            >
              {#each tiers as tier (tier.name)}
                <option value={tier.name}>{tier.name}</option>
              {/each}
            </select>
            <!-- Guardian for this admission ("None" ⇒ an ordinary account).
                 Local UI state until Approve, which passes it as the request's
                 `guardian_actor` (family-safety.md § Wire & data shape). -->
            <select
              data-testid={IDS.INVITE_REQUEST_ROW_GUARDIAN_SELECT}
              data-index={i}
              value={guardianOptionAt(requestGuardianIdx[req.id] ?? 0)}
              onchange={(e) => {
                const idx = (e.currentTarget as HTMLSelectElement).selectedIndex;
                requestGuardianIdx[req.id] = idx;
                // A band presupposes a guardian: clearing one clears the other.
                if (!guardianActorAt(idx)) requestAgeBand[req.id] = ageBandNotSet;
              }}
            >
              {#each guardians.options as opt}<option value={opt}>{opt}</option>{/each}
            </select>
            <!-- The applicant's recorded age claim — total: "No app age
                 verification" when none (absence is the signal, D6). -->
            <span class="muted small" data-testid={IDS.INVITE_REQUEST_ROW_AGE_CLAIM} data-index={i}
              >{resolveLocalizedNested(ageClaimLabel(req.age_band, req.age_band_provenance))}</span>
            <select
              data-testid={IDS.INVITE_REQUEST_ROW_AGE_BAND_SELECT}
              data-index={i}
              aria-label={t.family.age_band.label}
              disabled={!guardianActorAt(requestGuardianIdx[req.id] ?? 0)}
              bind:value={requestAgeBand[req.id]}
            >
              {#each ageBandOptionsList as opt (opt.value)}
                <option value={opt.value}>{resolveLocalizedNested(opt.label)}</option>
              {/each}
            </select>
            <button
              class="btn"
              data-testid={IDS.INVITE_REQUEST_ROW_APPROVE_BUTTON}
              data-index={i}
              onclick={() => approve(req)}>{t.admin.invite_requests_page.approve}</button>
            <button
              class="btn danger"
              data-testid={IDS.INVITE_REQUEST_ROW_DENY_BUTTON}
              data-index={i}
              onclick={() => deny(req)}>{t.admin.invite_requests_page.deny}</button>
            <input
              class="reason"
              data-testid={IDS.INVITE_REQUEST_ROW_DENY_REASON_FIELD}
              data-index={i}
              bind:value={denyReasons[req.id]}
              placeholder={t.admin.invite_requests_page.deny_reason_placeholder}
            />
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <!-- Section 2 — Registration: the nest's posture + the orthogonal free-tier
       ceiling, saved together by one set_registration_mode call (admin.md § 2).
       When this client cannot name the posture the controls are replaced by a
       read-only note — never a Save that would write our guess over the nest's
       real setting. -->
  <section data-testid={IDS.ADMIN_USERS_REGISTRATION_SECTION} class="section">
    <h2>{t.admin.users_page.section_registration}</h2>
    {#if registrationMode === null}
      <p class="muted">{t.admin.users_page.registration_mode_unknown({ mode: unknownMode ?? '—' })}</p>
    {:else}
      <label class="field">
        <span>{t.admin.users_page.registration_mode_label}</span>
        <select data-testid={IDS.ADMIN_USERS_REGISTRATION_MODE_SELECT} bind:value={registrationMode}>
          {#each registrationModeOptionsList as opt}
            <option value={opt.value}>{resolveLocalized(opt.label)}</option>
          {/each}
        </select>
      </label>
      <label class="field">
        <span>{t.admin.users_page.max_free_users_label}</span>
        <input data-testid={IDS.ADMIN_USERS_MAX_FREE_USERS_INPUT} bind:value={maxFreeUsers} />
      </label>
      <p class="muted">{t.admin.users_page.max_free_users_hint}</p>
      <!-- The age require-knob: a draft until the section's one Save. -->
      <label class="field">
        <input
          type="checkbox"
          data-testid={IDS.ADMIN_USERS_REGISTRATION_AGE_VERIFICATION_TOGGLE}
          data-state={ageVerificationDraft ? 'on' : 'off'}
          bind:checked={ageVerificationDraft}
        />
        <span>{t.admin.users_page.age_verification_required_label}</span>
      </label>
      <button
        class="btn"
        data-testid={IDS.ADMIN_USERS_REGISTRATION_SAVE_BUTTON}
        onclick={saveRegistration}>{t.admin.users_page.registration_save}</button>
    {/if}
  </section>

  <!-- Section 3 — Admit: direct admission (admin.md § 2; public-mode.md §
       Registration & Identity) — the third account-creation path, one
       fauna.admin.users.create call. A blank handle admits the deliberate
       handle-less state (public-mode.md § A handle-less account) — there is
       no set-later, only clear-later. -->
  <section data-testid={IDS.ADMIN_USERS_ADMIT_SECTION} class="section">
    <h2>{t.admin.users_page.section_admit}</h2>
    <label class="field">
      <span>{t.admin.users_page.admit_actor_label}</span>
      <input data-testid={IDS.ADMIN_USERS_ADMIT_ACTOR_INPUT} bind:value={admitActorInput} />
    </label>
    <label class="field">
      <span>{t.admin.users_page.admit_handle_label}</span>
      <input data-testid={IDS.ADMIN_USERS_ADMIT_HANDLE_INPUT} bind:value={admitHandleInput} />
    </label>
    <select data-testid={IDS.ADMIN_USERS_ADMIT_TIER_SELECT} bind:value={admitTier}>
      {#each tiers as tier (tier.name)}
        <option value={tier.name}>{tier.name}</option>
      {/each}
    </select>
    <button class="btn" data-testid={IDS.ADMIN_USERS_ADMIT_BUTTON} onclick={admitUser}
      >{t.admin.users_page.admit_button}</button>
  </section>

  <!-- Section 4 — Invite -->
  <section data-testid={IDS.ADMIN_USERS_INVITE_SECTION} class="section">
    <h2>{t.admin.users_page.section_invite}</h2>
    <button class="btn" data-testid={IDS.CREATE_INVITE_CODE_BTN} onclick={() => (showCreateForm = true)}
      >{t.admin.settings_page.create_code}</button>

    {#if showCreateForm}
      <div class="create-form" data-testid={IDS.ADMIN_SETTINGS_INVITE_CREATE_FORM}>
        <select data-testid={IDS.ADMIN_SETTINGS_TIER_SELECT} bind:value={newCodeTier}>
          {#each tiers as tier (tier.name)}
            <option value={tier.name}>{tier.name}</option>
          {/each}
        </select>
        <label for="admin-settings-max-uses-input-el">{t.admin.settings_page.max_uses}</label>
        <input
          id="admin-settings-max-uses-input-el"
          type="number"
          min="1"
          data-testid={IDS.ADMIN_SETTINGS_MAX_USES_INPUT}
          bind:value={newCodeMaxUses}
        />
        <!-- Guardian for the account that redeems this code ("None" ⇒ ordinary).
             Rides the code's `guardian_actor` (family-safety.md § Wire & data
             shape); onboarding shows the designation before redemption
             (invite-code-supervised-notice). -->
        <span class="caption">{t.admin.users_page.guardian_label}</span>
        <select
          data-testid={IDS.ADMIN_USERS_INVITE_GUARDIAN_SELECT}
          value={guardianOptionAt(newCodeGuardianIdx)}
          onchange={(e) => {
            newCodeGuardianIdx = (e.currentTarget as HTMLSelectElement).selectedIndex;
            // A band presupposes a guardian: clearing one clears the other.
            if (!guardianActorAt(newCodeGuardianIdx)) newCodeAgeBand = ageBandNotSet;
          }}
        >
          {#each guardians.options as opt}<option value={opt}>{opt}</option>{/each}
        </select>
        <span class="caption">{t.family.age_band.label}</span>
        <select
          data-testid={IDS.ADMIN_USERS_INVITE_AGE_BAND_SELECT}
          aria-label={t.family.age_band.label}
          disabled={!guardianActorAt(newCodeGuardianIdx)}
          bind:value={newCodeAgeBand}
        >
          {#each ageBandOptionsList as opt (opt.value)}
            <option value={opt.value}>{resolveLocalizedNested(opt.label)}</option>
          {/each}
        </select>
        <button class="btn" data-testid={IDS.CREATE_INVITE_CONFIRM_BTN} onclick={mintCode}>{t.common.confirm}</button>
        <button
          class="btn"
          data-testid={IDS.ADMIN_SETTINGS_INVITE_CANCEL_BUTTON}
          onclick={() => (showCreateForm = false)}>{t.common.cancel}</button>
      </div>
    {/if}

    {#if mintedCode}
      <p class="minted">
        {t.admin.users_page.code_minted}
        <button class="btn" data-testid={IDS.ADMIN_USERS_INVITE_CODE_COPY_BTN} onclick={() => navigator.clipboard?.writeText(mintedCode)}
          ><code>{mintedCode}</code> {t.common.copy}</button>
      </p>
    {/if}

    {#if inviteCodes.length === 0}
      <p class="muted">{t.admin.settings_page.no_codes}</p>
    {:else}
      <ul class="list">
        {#each inviteCodes as code, i (code.code)}
          <li class="row" data-testid={IDS.INVITE_CODE_ITEM} data-index={i}>
            <code data-testid={IDS.INVITE_CODE_VALUE}>{code.code}</code>
            <span class="tier-badge">{code.tier}</span>
            <span class="muted small">{t.admin.settings_page.uses_left_n({ count: String(code.uses_left) })}</span>
            <!-- The minted band echoes on the same row (no new id). -->
            {#if code.age_band}
              {@const band = ageBandLabel(code.age_band)}
              {#if band}<span class="tier-badge">{resolveLocalized(band)}</span>{/if}
            {/if}
            <button
              class="btn danger small"
              data-testid={IDS.ADMIN_SETTINGS_INVITE_DELETE_BUTTON}
              data-index={i}
              onclick={() => deleteCode(code.code)}>{t.common.delete}</button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <!-- Section 5 — Users -->
  <section data-testid={IDS.ADMIN_USERS_LIST_SECTION} class="section">
    <h2>{t.admin.users_page.section_users}</h2>
    <p class="muted" data-testid={IDS.USER_COUNT_TEXT}>{t.admin.users_page.user_count({ count: String(total) })}</p>
    {#if users.length === 0}
      <p class="muted">{t.admin.users_page.no_users}</p>
    {:else}
      <ul class="list">
        {#each users as user, i (actorHex(user.actor_id))}
          {@const controls = adminUserRowControls(user)}
          <li class="row" data-testid={IDS.USER_ROW} data-index={i}>
            <code class="actor" data-testid={IDS.USER_ACTOR_ID} title={actorHex(user.actor_id)}
              >{shortId(actorHex(user.actor_id))}</code>
            {#if user.label}<span class="label">{user.label}</span>{/if}
            <!-- Read-only IMAP/CalDAV-serving audit indicator (admin sees it,
                 user controls it from their own serve-here toggle). Default-on:
                 absent/true ⇒ "Serving here". No admin write path. -->
            <span
              class="serving-status"
              class:off={user.mail_serving_enabled === false}
              data-testid={IDS.ADMIN_USERS_MAIL_SERVING_STATUS}
              data-index={i}
            >{resolveLocalized(mailServingStatusLabel(user.mail_serving_enabled !== false))}</span>
            <select
              data-testid={IDS.ADMIN_USERS_TIER_SELECT}
              data-index={i}
              bind:value={userTier[actorHex(user.actor_id)]}
              onchange={() => changeTier(user)}
            >
              {#each tiers as tier (tier.name)}
                <option value={tier.name}>{tier.name}</option>
              {/each}
            </select>
            <!-- Lifecycle controls (admin.md § 2 Users → *Cutting a user off*). WHICH of
                 the three a row offers is the shared decision — `adminUserRowControls` over
                 wasm, unit-tested in `fauna-client-admin` — never re-derived here. The old
                 `user.eviction ? restore : evict` branch was exactly the bug that rule
                 exists to prevent: it made Suspend-from-`warning` unreachable. One click
                 acts immediately (no confirm), since both cut-off paths are reversible by
                 the restore control beside them. -->
            {#if controls.evict}
              <button
                class="btn danger"
                data-testid={IDS.ADMIN_USERS_EVICT_BUTTON}
                data-index={i}
                onclick={() => evict(user)}>{t.admin.users_page.evict}</button>
            {/if}
            {#if controls.suspend}
              <button
                class="btn danger"
                data-testid={IDS.ADMIN_USERS_SUSPEND_BUTTON}
                data-index={i}
                onclick={() => suspend(user)}>{t.admin.users_page.suspend}</button>
            {/if}
            {#if controls.restore}
              <button
                class="btn"
                data-testid={IDS.ADMIN_USERS_CANCEL_EVICTION_BUTTON}
                data-index={i}
                onclick={() => cancelEvict(user)}>{t.admin.users_page.cancel_eviction}</button>
            {/if}
            {#if controls.make_admin}
              <button
                class="btn"
                data-testid={IDS.ADMIN_USERS_MAKE_ADMIN_BUTTON}
                data-index={i}
                onclick={() => makeAdmin(user)}>{t.admin.users_page.make_admin}</button>
            {/if}
            {#if controls.remove_admin}
              <button
                class="btn danger"
                data-testid={IDS.ADMIN_USERS_REMOVE_ADMIN_BUTTON}
                data-index={i}
                onclick={() => removeAdmin(user)}>{t.admin.users_page.remove_admin}</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    <!-- Pagination (admin.md § Users — limit/offset on users.list). Reflect the
         bound on the buttons themselves (convention 11: an actuation route must
         not drive a control the UI has disabled) — the click handlers already
         guard via the same prevPageOffset/nextPageOffset, so this is visual
         feedback only, matching linux's set_sensitive(offset > 0)-style guard. -->
    <div class="pagination" data-testid={IDS.ADMIN_USERS_PAGINATION}>
      <button
        class="btn"
        data-testid={IDS.ADMIN_USERS_PREV_PAGE}
        disabled={prevPageOffset(offset, PAGE_SIZE) === undefined}
        onclick={prevPage}>{t.admin.users_page.prev_page}</button>
      <span class="muted small">{currentPage(offset, PAGE_SIZE)} / {totalPages(total, PAGE_SIZE)}</span>
      <button
        class="btn"
        data-testid={IDS.ADMIN_USERS_NEXT_PAGE}
        disabled={nextPageOffset(offset, total, PAGE_SIZE) === undefined}
        onclick={nextPage}>{t.admin.users_page.next_page}</button>
    </div>
  </section>
{/if}

<style>
  h2 {
    margin-top: 1.5rem;
    font-size: 1rem;
  }
  .section {
    margin-bottom: 1.5rem;
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
  .actor {
    font-family: monospace;
    font-size: 0.85rem;
  }
  .tier-badge {
    font-size: 0.75rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-hover, #1c2128);
  }
  .serving-status {
    font-size: 0.75rem;
    color: var(--text-muted, #8b949e);
  }
  .serving-status.off {
    color: var(--color-error, #f85149);
  }
  .create-form {
    display: flex;
    gap: 0.5rem;
    margin: 0.5rem 0;
    flex-wrap: wrap;
    align-items: center;
  }
  .caption {
    font-size: 0.8rem;
    color: var(--text-muted, #8b949e);
  }
  .pagination {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-top: 0.75rem;
  }
  .reason {
    flex: 1;
    min-width: 8rem;
  }
</style>
