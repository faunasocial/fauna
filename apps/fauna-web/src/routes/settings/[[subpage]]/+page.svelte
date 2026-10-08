<script lang="ts">
  import { rejectionText } from '$lib/rejection';
  import { identity } from '$lib/store';
  import { copyAndConfirm } from '$lib/copy-confirm';
  import { aftermathProgress, configStageSettled } from '$lib/succession-aftermath';
  import {
    accountsList,
    accountsSwitch,
    accountsSwitchConfirmed,
    accountsSetRequireConfirm,
    accountsRemove,
    type AccountEntry,
  } from '$lib/accounts';
  import { base } from '$app/paths';
  import { pushOptedIn, enablePush, disablePush, dropActorPushRow } from '$lib/push';
  import {
    ensureWasm,
    encodeEmailFilterRule,
    encodeEmailFilterActionInputs,
    defaultFilterActionInputs,
    FILTER_ACTION_KINDS,
    type FilterActionInputs,
    describeEmailFilterRule,
    describeEmailFilterActionInputs,
    emailFilterActionLabel,
    filterIsEditableFor,
    spamThresholdBand,
    inboxModeValues,
    validateHandle,
    describePendingAction,
    ensureLogging,
    logSnapshot,
    logClear,
    identityQrEncode,
    type LogEntry,
  } from '$lib/wasm';
  import { qrDisplayFromUri, type QrDisplay } from '$lib/qr-display';
  import {
    authHeaders,
    nodeUrl,
    storedNestUrl,
    fetchQuota,
    fetchFeatures,
    changeHandle,
    deleteAccount,
    pendingActionsList,
    pendingActionCancel,
    type PendingActionSummary,
    recoveryKitStatus,
    createRecoveryKit,
    replaceRecoveryKit,
    requestRecoveryKitLost,
    vetoRecoveryReplacement,
    resealRecoveryEscrow,
    succeedIdentityWithHeldKit,
    getInboxMode,
    setInboxMode,
    listEmailFilters,
    createEmailFilter,
    getEmailFilter,
    updateEmailFilter,
    deleteEmailFilter,
    filterMarksList,
    filterMarkKeep,
    filterMarkRemoved,
    getSpamPreferences,
    updateSpamPreferences,
    checkIsAdmin,
    type QuotaInfo,
    type FeatureRow,
    type EmailFilter,
    type RecoveryStatus,
    type RecoveryMinted,
  } from '$lib/api';
  import {
    getConversationsManager,
    scheduleMlsSave,
    KEYPACKAGE_TARGET,
    memberReviewList,
    memberReviewRowText,
    memberReviewHandleForPerson,
    memberReviewKeep,
    memberReviewRemove,
    type MemberReview,
    type MemberReviewRowText,
    type CrossGroupEviction,
  } from '$lib/conversations';
  import { ephemeralReviewPassActive, deferEphemeralReviewPass, successionSweepCopy, successionSweepRetry } from '$lib/rpc';
  // The owed-kit slot's three faces. From `$lib/wasm`, not `$lib/rpc`, because
  // they are pure sessionStorage reads that need no connected client — see
  // their doc comments for why the sibling succession reads beside them do.
  import { claimOwedSuccessionKit, rearmOwedSuccessionKit, dischargeOwedSuccessionSweep } from '$lib/wasm';
  import { dischargeOwedKit } from '$lib/succession-kit';
  import { RecoveryErrorGuard } from '$lib/recovery-error-guard';
  import { ownSupersessionHold } from '$lib/own-supersession-hold';
  import { performHeldBackSupersession } from '$lib/post-auth-escalation';
  import type { SweepCopy } from '$lib/rpc';
  import {
    keypackageCount as wsKeypackageCount,
    moderationActions,
    moderationAbuseReportMine,
    moderationAbuseReportWithdraw,
    mutedKeywordsList,
    mutedKeywordsAdd,
    mutedKeywordsRemove,
    type MutedWordsSnapshot,
    type ObligationAction,
  } from '$lib/rpc';
  import {
    conversationsSnapshot,
    moderationLocalDetections,
    trainModerationCorrection,
  } from '$lib/conversations';
  import {
    accountDisplayLabel,
    confidencePercent,
    moderationQueue,
    obligationActionLabel,
    quotaPercent,
    shortId,
    reportLedgerWords,
    reportWithdrawVerdict,
    type ReportLedgerRow,
    type LocalDetection,
    type QueueRow,
  } from '$lib/wasm';
  import { resolveLocalized, resolveLocalizedNested, cellValueText } from '$lib/i18n/localized';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { onDestroy, onMount } from 'svelte';
  import { onStoreChange } from '$lib/store-change';
  // The already-built conversations manager, for the succession ceremony's
  // post-succession group sweep. Never builds one — see `doSucceedIdentity`.
  import {
    conversationsManagerIfReady,
    removeAccountBlocked,
    signOutBlockedByAnotherTab,
  } from '$lib/conversations';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import NestsSection from '$lib/components/NestsSection.svelte';
  import RegionSettingsSection from '$lib/components/RegionSettingsSection.svelte';
  import TaskDelegationSection from '$lib/components/TaskDelegationSection.svelte';
  import ConnectedAppsSection from '$lib/components/ConnectedAppsSection.svelte';
  import MailSettingsSection from '$lib/components/MailSettingsSection.svelte';
  import MailAliasesSection from '$lib/components/MailAliasesSection.svelte';
  import MailListsSection from '$lib/components/MailListsSection.svelte';
  import MailSpamSection from '$lib/components/MailSpamSection.svelte';
  import MailExportSection from '$lib/components/MailExportSection.svelte';
  import MailImportSection from '$lib/components/MailImportSection.svelte';
  import WebSettingsSection from '$lib/components/WebSettingsSection.svelte';
  import SubscriptionsSection from '$lib/components/SubscriptionsSection.svelte';
  import NostrSettingsSection from '$lib/components/NostrSettingsSection.svelte';
  import AtprotoSettingsSection from '$lib/components/AtprotoSettingsSection.svelte';
  import DevicesSection from '$lib/components/DevicesSection.svelte';
  import FoldersSection from '$lib/components/FoldersSection.svelte';
  import PersonalizationSection from '$lib/components/PersonalizationSection.svelte';
  import LabelerCatalogSection from '$lib/components/LabelerCatalogSection.svelte';
  import LogsView from '$lib/components/LogsView.svelte';
  import ContentLabelBadge from '$lib/components/ContentLabelBadge.svelte';
  import { t } from '$lib/i18n/strings';
  import { byteSize } from '$lib/value-format';
  import { IDS } from '$lib/generated/uiIds';

  // The Settings sidebar-swap shell (settings.md § Navigation model): the rail
  // (routes/settings/+layout.svelte) switches sub-pages by URL — /app/settings
  // (Status, default) and /app/settings/<id> for the rest. This ONE component
  // holds the shared fetch + handlers (onMount → loadAll fetches quota / inbox /
  // filters / spam / keys together) and renders exactly the selected sub-page,
  // so cross-sub-page navigation is a param change (component stays mounted, no
  // re-fetch) rather than 15 routes each re-running the shared load.
  //
  // "status" normalizes to the bare root: linux/windows/tui all accept the
  // explicit sub-id "status" as a synonym for the default landing page (the
  // e2e action layer's uniform two-element nav sends
  // {"view":"settings","id":"status"} for the Status sub-page across every
  // app), so web must land on the same content for either form.
  let rawSubpage = $derived($page.params.subpage ?? '');
  let current = $derived(rawSubpage === 'status' ? '' : rawSubpage);

  // The shared page-heading (settings.md § Sub-page heading conformance) must
  // read the SUB-PAGE's own title, not the bare "Settings" shell title — a
  // sub-page with its own e2e-asserted title gets a branch here; anything
  // else (including the default Status landing) keeps the shell title.
  let pageHeadingText = $derived(
    current === 'account' ? t.settings.account_page.title
    : current === 'privacy' ? t.settings.privacy_page.title
    : current === 'encryption' ? t.settings.encryption_page.title
    : current === 'logs' ? t.logs.title
    : current === 'connected-apps' ? t.connected_apps.title
    : t.common.settings
  );

  let loading = $state(true);
  let error = $state('');
  let deleteConfirmText = $state('');
  let deleteAccountError = $state('');
  let actorIdCopied = $state(false);
  let nodeUrlCopied = $state(false);
  // What the last click actually put on the clipboard — painted as the
  // buttons' `data-copied` (the copy-button contents contract; the label
  // swap above is timed, this is not).
  let copiedActorId = $state<string | null>(null);
  let copiedNodeUrl = $state<string | null>(null);

  // Account management state
  let quota: QuotaInfo | null = $state(null);
  // `feature-limits-section` — null (not `[]`) is the "read hasn't resolved
  // yet" state, so the section renders nothing until the real
  // `fauna.features.status` reply arrives (`settings.md` § Layout & flow item
  // 2b: "renders only once the transparency read resolves").
  let features: FeatureRow[] | null = $state(null);
  let newHandle = $state('');
  let changeHandleError = $state('');
  let changingHandle = $state(false);

  // Multi-account switcher (long-term-store.md § Multi-account evolution). The
  // shared registry (`fauna_client_accounts` over wasm) owns the list + active
  // pointer + migration; this page renders the rows + drives switch/add/remove.
  let accounts = $state<AccountEntry[]>([]);
  // "The account in use" is the one THIS TAB serves — its signed-in identity,
  // which follows the per-tab pin — never the registry's active pointer: a
  // pinned tab serves a non-active account, and keying on the pointer offered
  // that account for removal from the very tab running on it
  // (`account-scoping.md` § Concurrent instances → *Remove-account also refuses
  // the account THIS instance serves*).
  let servedActor = $derived($identity?.actorId?.toLowerCase());
  let switchingAccount = $state(false);
  let accountsError = $state('');

  async function loadAccounts() {
    try {
      accounts = await accountsList();
    } catch (e) {
      accountsError = String(e);
    }
  }

  // Stage-2 in-app re-auth prompt target (`account-activate-reauth-prompt`,
  // long-term-store.md § Multi-account evolution → Per-account re-auth). Set
  // while the prompt is up; null otherwise. Web has no native OS re-auth prompt,
  // so it renders the in-app shape the linux slice ratified.
  let reauthTarget = $state<{ actorId: string; label: string } | null>(null);

  /** The Stage-2 gate in front of the switch seam: resolve the re-auth question
   *  BEFORE anything app-owned runs, so `performSwitch`'s mutation-first order
   *  (registry write before the reload/teardown) holds unchanged. The flag is
   *  read FRESH from the registry, never off the rendered row: the admin
   *  auto-default can write it after this page loaded its list, and a stale row
   *  would skip the prompt (the registry would still refuse — fail-closed, but a
   *  dead-feeling click). Both activation paths route through here. */
  async function requestSwitchAccount(actorId: string) {
    if (switchingAccount || actorId.toLowerCase() === servedActor) return;
    accountsError = '';
    let flagged: AccountEntry | undefined;
    try {
      flagged = (await accountsList()).find(
        (a) => a.actor_id === actorId && a.require_confirm_to_activate,
      );
    } catch (e) {
      accountsError = String(e);
      return;
    }
    if (flagged) {
      reauthTarget = {
        actorId,
        label: accountDisplayLabel(flagged.handle, flagged.actor_id),
      };
      return;
    }
    await performSwitch(actorId, false);
  }

  async function performSwitch(actorId: string, confirmed: boolean) {
    if (switchingAccount) return;
    switchingAccount = true;
    accountsError = '';
    try {
      const outgoing = $identity;
      // set_active + re-pin this tab, then a full reload re-runs the launch
      // flow against the now-active identity, read back from the registry (web's teardown+rebuild; the admin shell reveals itself via the
      // per-actor `am-i-admin` check). No relaunch UI — the browser reloads.
      // `confirmed` is the Stage-2 re-auth bit: plain set_active REFUSES a
      // flagged account, so a path that skipped the prompt fails loudly here —
      // nothing has been torn down yet.
      if (confirmed) {
        await accountsSwitchConfirmed(actorId);
      } else {
        await accountsSwitch(actorId);
      }
      // The leave-gesture push drop (`common.md` § Registration): the
      // OUTGOING actor's row falls while its authority is still in hand —
      // awaited (not fire-and-forget) because the hard reload below would
      // kill an in-flight call; best-effort inside the helper, so an offline
      // switch still switches. The incoming actor re-arms via the reload's
      // `rearmPush`. AFTER the activation, never before: a
      // refused switch leaves the user where they were, and that includes
      // their notifications (`multiple-accounts` outcome 11).
      if (outgoing) await dropActorPushRow(outgoing.secretHex);
      // Count the teardown at its INITIATION point — synchronously, in the same
      // job as the navigation below, never after it (the navigation never
      // returns). Convention 14's negative-assert observable
      // (`fauna_e2e_agent::SESSION_GENERATION_KEY`); the write is a
      // `sessionStorage` bump precisely so it survives the document swap.
      //
      // Reached through the installed `window.__fauna_*` hook rather than a
      // static `$lib/generation-e2e` import, so convention 15 holds in the
      // strong form the other hooks have: a production build constant-folds
      // this branch away AND never bundles the module, instead of shipping it
      // as dead code behind a false flag. A dynamic import here would be wrong
      // for a different reason — it resolves a microtask later, and the
      // navigation below does not wait.
      if (__FAUNA_E2E_AUTOMATION__) {
        (window as unknown as { __fauna_recordSessionTeardown?: () => void })
          .__fauna_recordSessionTeardown?.();
      }
      // Raw navigation (full document reload) — `base` ('/app') + the route, so
      // /app/feed. A hard reload re-runs the launch flow against the now-active
      // identity; a soft `goto` would not rebuild the wasm WS-RPC/MLS session.
      window.location.assign(`${base}/feed`);
    } catch (e) {
      accountsError = rejectionText(e, t.settings.switch_refused({ account: actorId }));
      switchingAccount = false;
    }
  }

  function confirmReauth() {
    const target = reauthTarget;
    reauthTarget = null;
    // The user just confirmed → the post-re-auth path. This is the ONLY
    // `confirmed = true` call site: the audit surface for the gate.
    if (target) void performSwitch(target.actorId, true);
  }

  /** Declining is a PURE no-op (ratified): no registry mutation, no teardown,
   *  no error banner — the user stays on the account they were already using.
   *  Cancel, backdrop click, and Escape all land here. */
  function cancelReauth() {
    reauthTarget = null;
  }

  /** The `account-require-confirm-toggle` write path. Only ever fired by a real
   *  user gesture (`onchange` never fires on a programmatic/prop re-render in
   *  Svelte — the structural guard linux needs a syncing flag for): the write
   *  marks the flag user-set, which pins the choice against the admin
   *  auto-default. Re-read the registry afterwards so the row renders the
   *  persisted truth. */
  async function setRequireConfirm(actorId: string, on: boolean) {
    accountsError = '';
    try {
      await accountsSetRequireConfirm(actorId, on);
    } catch (e) {
      accountsError = String(e);
    }
    await loadAccounts();
  }

  async function removeAccount(actorId: string) {
    // Only offered on rows other than the one this tab serves, so removal never
    // touches the identity this tab runs on. It still refuses — nothing erased —
    // when that is somehow the target, or when another tab serves the account
    // (`account-scoping.md` § Concurrent instances → *An erase refuses while a
    // sibling serves the account*), asked before the registry removal that drops
    // the account's secret slots.
    accountsError = '';
    const blocked = await removeAccountBlocked(actorId);
    if (blocked === 'this_tab') {
      accountsError = t.settings.remove_account_blocked_this_window;
      return;
    }
    if (blocked === 'other_tab') {
      accountsError = t.settings.remove_account_blocked_other_window;
      return;
    }
    try {
      await accountsRemove(actorId);
      await loadAccounts();
    } catch (e) {
      accountsError = String(e);
    }
  }

  function addAccount() {
    // Append-mode onboarding: a fresh create-or-import wizard that, on success,
    // registers the new identity + switches to it (rather than overwriting the
    // single session). `?add=1` is the append-mode flag the onboarding page reads.
    //
    // The leave-gesture push drop happens HERE, not at the wizard's own
    // `accountsSwitch` — the append flow is the one switch commit that no
    // longer holds the outgoing authority (`common.md` § Registration).
    // Fire-and-forget: `goto` is a soft nav, so the call survives it, and an
    // abandoned wizard self-heals — reconcile re-arms the still-active actor
    // on its next identity settle.
    const outgoing = $identity;
    if (outgoing) void dropActorPushRow(outgoing.secretHex);
    goto('/app/onboarding?add=1');
  }
  // Inline sign-out confirm (uniform with admin-factory-reset-confirm-button —
  // drivable by e2e, unlike the old native confirm()). settings.md § User actions.
  let showSignOutConfirm = $state(false);

  // `qrDisplayFromUri` / `QrDisplay` live in `$lib/qr-display` — shared with
  // onboarding's recovery-kit offer.

  // Identity export (settings.md § Identity export). `null` = collapsed: the QR and its
  // warning render only while this is set, so the secret is never on screen by accident
  // when a user opens Settings (e.g. while sharing a screen). Pure view state — nothing is
  // persisted and no server call is made. Hiding drops the derived modules, so the encoded
  // secret isn't retained any longer than it is on screen.
  let identityQr = $state<QrDisplay | null>(null);

  async function toggleIdentityQr() {
    if (identityQr) {
      identityQr = null;
      return;
    }
    const id = $identity;
    if (!id) return;
    await ensureWasm();
    // Both halves are shared Rust: `identityQrEncode` builds exactly the URI the import
    // parser accepts, `qrMatrix` turns it into the boolean grid every app draws.
    identityQr = qrDisplayFromUri(identityQrEncode(id.secretHex, id.handle));
  }

  // Recovery kit (settings.md § Recovery kit) — the RecoveryKey's Settings
  // home, immediately after Identity export. tui shipped this section first
  // (apps/fauna-tui/src/settings/recovery.rs); linux ported the 5-element
  // core family alongside this row (apps/fauna-linux/src/settings/
  // recovery_kit.rs) — same scope here: section/status/create/replace/lost
  // plus the phrase entry (replace only) and the minted-kit display trio.
  let recoveryStatus = $state<RecoveryStatus | null>(null);
  let recoveryBusy = $state(false);
  /** The section's one error surface (rule A: one `error-message` per page).
   *  ⚠ Never assign this directly — every writer, INCLUDING the parking write
   *  in `doSucceedIdentity`'s persist-failure arm, MUST go through
   *  `recoveryErrorGuard` (`$lib/recovery-error-guard`): the guard drops
   *  every ordinary write while a stolen-ceremony persist-failure message is
   *  parked, because that message is the ONLY surviving copy of the
   *  successor's identity secret (`settings.md` § Recovery kit → *The
   *  persist-failure message survives the page*). Mirrors linux's
   *  `PENDING_STOLEN_FAILED_MESSAGE` and apple's `stolenFailedMessagePending`. */
  let recoveryError = $state('');
  const recoveryErrorGuard = new RecoveryErrorGuard();
  let recoveryPhrase = $state('');
  /** `identity-stolen-confirm-field` — the type-to-confirm gate on the
   *  succession ceremony, the same idiom account deletion uses
   *  (settings.md § Recovery kit). The token is NOT localized (only its prompt
   *  is), so the comparison is against the literal on every app and in every
   *  language. */
  let stolenConfirm = $state('');
  // The shown-once minted kit. `null` = nothing to show; dropped on the next
  // ceremony's result or on leaving the page — there is no "show it again"
  // path and there can never be one (identity-succession.md § The
  // RecoveryKey — Custody).
  let recoveryMinted = $state<{ secretHex: string; uri: string } | null>(null);
  let recoveryQr = $state<QrDisplay | null>(null);
  // The seven aftermath-progress lines (built the
  // render; supplied the pass that fills them). Each field is the
  // shared projection's OWN `status_line()` result, already resolved to a
  // `LocalizedText` — never re-derived here, so this page cannot drift from
  // tui/linux's copy.
  //
  // ⚠ Read from a MODULE store, not page state, and that is load-bearing: the
  // pass is started from the root layout at the actor settle after a
  // successor signs in, while this sub-page mounts later, when the user walks
  // to it. Page state would be null at exactly the moment the surface exists
  // to be read. See `$lib/succession-aftermath`.
  //
  // Two fields stay null on web today and say so where they are declared: the
  // `__mls` leg (a barrier inside the conversations replica's own load) and
  // the file-corpus leg (the sync agent's, on no app).
  let recoveryAftermath = $derived($aftermathProgress);

  // Pending actions (settings.md § Pending actions) — the STANDING account-
  // page section listing the cancellable window the three delayed verbs
  // (handle change, account delete, snapshot delete) open. tui shipped this
  // first (apps/fauna-tui/src/settings/account.rs::pending_actions_elements);
  // linux ported the same shape (apps/fauna-linux/src/settings/
  // pending_actions.rs). `null` = not yet hydrated (bare title); `[]` =
  // hydrated and empty; non-empty = counted title with rows — never a
  // settled "nothing scheduled" claim before the first list read lands.
  let pendingActions = $state<PendingActionSummary[] | null>(null);
  let pendingActionsError = $state('');

  async function loadPendingActions() {
    const id = $identity;
    if (!id) return;
    try {
      pendingActions = await pendingActionsList(id.secretHex);
      pendingActionsError = '';
    } catch (e) {
      // The section keeps its bare title on a failed read — no basis for any
      // other claim.
      pendingActionsError = e instanceof Error ? e.message : String(e);
    }
  }
  $effect(() => {
    if (current === 'account') void loadPendingActions();
  });

  /** `pending-action-cancel-button` — one click, no confirm: cancelling is
   *  the safe direction. Ends on a fresh list read, never a local splice, so
   *  the row count always reflects the nest's own state. */
  async function doCancelPendingAction(id: number) {
    const identityValue = $identity;
    if (!identityValue) return;
    try {
      await pendingActionCancel(identityValue.secretHex, id);
      await loadPendingActions();
    } catch (e) {
      pendingActionsError = e instanceof Error ? e.message : String(e);
    }
  }

  /** The SPA's already-qualified `handle@domain` for the recovery URI's
   *  handle half (settings.md § Recovery kit — "QUALIFIED with the nest's
   *  host... a bare local part in a payload read months later on a device
   *  that has never seen this account names nothing"), or `null` before a
   *  handle is chosen. Unlike identity-export's bare handle, recovery kit
   *  needs the domain too — the SPA already tracks it separately. */
  function qualifiedRecoveryHandle(id: { handle?: string | null; domain?: string | null }): string | null {
    if (!id.handle) return null;
    return id.domain ? `${id.handle}@${id.domain}` : id.handle;
  }

  async function loadRecoveryStatus() {
    const id = $identity;
    if (!id) return;
    try {
      recoveryStatus = await recoveryKitStatus(id.secretHex);
    } catch (e) {
      recoveryError = recoveryErrorGuard.write(recoveryError, e instanceof Error ? e.message : String(e));
    }
  }
  $effect(() => {
    if (current !== 'account') return;
    // ⚠ ENTERING the section clears any kit on screen — the shown-once custody
    // rule (`identity-succession.md` § The RecoveryKey → *Custody*: "displayed
    // once … never stored on any device"). tui and apple have always done this
    // because their sections are rebuilt on entry; web's is NOT — cross-sub-page
    // navigation here is a param change that leaves the component mounted (see
    // `rawSubpage`), so a minted secret survived every navigation for the life
    // of the tab and came back on screen next time Account was opened. A kit is
    // shown once, to the person who asked for it, in that moment — which is
    // what `recoveryMinted`'s own declaration already promised ("dropped on
    // leaving the page — there is no 'show it again' path") without anything
    // making it true. Two independent fixes for this landed the same day; the
    // wider one is kept, because the display renders on
    // `recoveryMinted && recoveryQr` and a surviving QR is the same secret in
    // another encoding.
    //
    // Declared ABOVE the discharge effect below, and that order is load-bearing:
    // effects run in declaration order, so the clear can never wipe a kit the
    // closing act just minted. It is the same ordering rule tui states as
    // "navigate synchronously, then mint".
    recoveryMinted = null;
    recoveryQr = null;
    void loadRecoveryStatus();
    // The section's inherited-filters line counts these, and a Keep answered
    // on the filter list since the last read must already be gone from it.
    void refreshFilterMarks();
  });
  // The discharge for a still-pending persist-failure message: the user has
  // navigated OFF the Account sub-page, having had the whole visit to read or
  // copy the successor's key (`settings.md` § Recovery kit → *The
  // persist-failure message survives the page*). A DEDICATED effect, never a
  // cleanup returned from the entry effect above: Svelte 5 runs an effect's
  // cleanup before EVERY re-run, not only on unmount, and the entry effect
  // above reads `$identity` (via `loadRecoveryStatus`) — a cleanup nested in
  // it would fire on every identity-store update (e.g. `store.ts`'s
  // background `registered: true` refresh), clearing the pending flag while
  // the user is still on the page. This effect reads `current` only —
  // `$identity` stays untracked — so it fires exactly on the nav edge away
  // from `account`, never on an unrelated identity-store write.
  //
  // The same edge performs a supersession escalation held back for this
  // device's own stolen-identity ceremony (`$lib/own-supersession-hold`, the
  // same section's closing rule): the user has read the key, so the dead
  // session it was parked over may now go the ordinary way.
  let wasOnAccountSubpage = current === 'account';
  function leaveAccount() {
    recoveryErrorGuard.discharge();
    if (ownSupersessionHold.leftAccount()) performHeldBackSupersession();
  }
  $effect(() => {
    const onAccount = current === 'account';
    if (wasOnAccountSubpage && !onAccount) leaveAccount();
    wasOnAccountSubpage = onAccount;
  });
  // Leaving Settings altogether is the same edge, but no `current` change
  // reaches the effect above — the component simply unmounts. The guard dies
  // with the component; the hold is module state and must hear it.
  onDestroy(() => {
    if (wasOnAccountSubpage) leaveAccount();
  });
  // Discharge (and this time also CLEAR) a still-pending persist-failure
  // message when the SIGNED-IN IDENTITY changes — a multi-account switch that
  // keeps this component mounted, per the entry effect's own comment above.
  // Mirrors apple's `resetForIdentityChange`, which discharges BEFORE its own
  // clearing write so that write is never itself dropped by the guard
  // (`RecoveryKitVM.swift`). Keyed on `actorId`, not on `$identity` firing at
  // all: `store.ts`'s background refresh reassigns the WHOLE identity object
  // on an unrelated field (`registered: true`) for the SAME actor, and that
  // must not discharge or clear anything.
  let previousRecoveryActorId: string | null = null;
  $effect(() => {
    const actorId = $identity?.actorId ?? null;
    if (previousRecoveryActorId !== null && actorId !== previousRecoveryActorId) {
      recoveryErrorGuard.discharge();
      recoveryError = '';
    }
    previousRecoveryActorId = actorId;
  });
  // The succession's CLOSING ACT, discharged from the section that renders it
  // (`identity-succession.md` § The RecoveryKey → *At succession*): the
  // successor's first authenticated session mints, registers, escrows and
  // **shows** a fresh kit, unbidden.
  //
  // Here rather than in the layout that navigated us, because the secret has to
  // land in the state this section paints from — apple's split exactly
  // (`RecoveryKitVM.dischargeOwedSuccessionKit`, called from the section's own
  // hydrate). The claim is take-and-clear, so this is one-shot however many
  // times the effect re-runs, and it is bound to THIS identity's actor id, so
  // a section rendering for a different seat can never take a kit owed to
  // someone else.
  $effect(() => {
    if (current === 'account') void dischargeOwedSuccessionKit();
  });

  function applyRecoveryMinted(result: RecoveryMinted) {
    recoveryStatus = result.status;
    recoveryMinted = { secretHex: result.secret_hex, uri: result.uri };
    recoveryQr = qrDisplayFromUri(result.uri);
    recoveryBusy = false;
  }

  /** Mint, register, escrow and SHOW the successor's fresh kit — unbidden, as
   *  the closing act of an identity succession.
   *
   *  **Why unbidden and not a prompt.** The succession transaction deletes the
   *  old `recovery_escrow` row and the old kit retires with the old identity,
   *  so the account has no kit and no escrow at all from the moment the
   *  statement lands until this runs — for a user who has just proven they are
   *  a theft target. A *silent* background mint cannot close that window
   *  either: the secret may never be persisted, so an unshown mint registers a
   *  kit **nobody holds**, leaving them strictly worse off than never-created
   *  (`identity-succession.md` § The RecoveryKey → *At succession*).
   *
   *  ⚠ **A failed mint re-arms the obligation, it never spends it.** The
   *  ceremony revokes every session of the account inside the nest's own
   *  transaction, so this mint races the successor's own reconnect — on every
   *  platform, not just this one (apple's cross-platform lesson: macOS passed
   *  on timing luck, iOS did not). Re-arming is safe because the mint is a
   *  replace — `create_kit` re-reads the chain head and picks its own arm — so
   *  a second mint supersedes a stranded first. */
  async function dischargeOwedSuccessionKit() {
    const id = $identity;
    // Every step of the closing act reports itself, and this one is why: the
    // chain crosses a document swap, and a break at ANY link presents
    // identically from the DOM (no kit, empty error surface). The journey's
    // failure diagnosis quotes this ring
    // (`test_identity_succession_ceremony.py::_closing_act_console`).
    console.debug(
      '[succession] account section up; identity=', !!id, 'registered=', !!id?.registered,
    );
    if (!id) return;
    // ⚠ NOT before the session is attached. tui states the rule at its own
    // discharge: the mint is spawned after `attach_session`, because the
    // minted-kit fold reads the session's actor id and handle to build the
    // `fauna://recovery` payload, and minting earlier produces a kit whose URI
    // names no account — the impoverished form the 2026-08-02 copy-button
    // ruling retired.
    //
    // `registered` is web's exact analogue and `store.ts` says so where it sets
    // it: it is "the live-session signal … the one point in the SPA that knows
    // the identity reached a WORKING session rather than merely being present
    // in localStorage". A fresh document has an identity from `localStorage`
    // within milliseconds and a *working* one only after the silent challenge
    // answers; discharging in that gap mints against half a session.
    //
    // Costs nothing when it is already true, and this effect re-runs on the
    // identity change that flips it — so the obligation is discharged the
    // moment the session is real, and never before.
    if (!id.registered) return;
    // A relaunch adoption also owes the group sweep its lost ceremony never ran
    // (`succession-propagation.md` § Propagation → *Own device fleet*, the
    // relaunch-adoption clause): an unbidden press of the sweep retry, ahead of
    // the kit. It parks the report its answer chooses — re-read here so the
    // sweep's lines and the retry button render — and says its answer as a
    // press would. `null` on every session that adopted nothing.
    try {
      const sweepAnswer = await dischargeOwedSuccessionSweep(id.actorId);
      if (sweepAnswer) {
        console.info('[succession] discharged the owed group sweep for', id.actorId);
        sweepCopy = await successionSweepCopy(id.secretHex);
        recoveryError = recoveryErrorGuard.write(recoveryError, resolveLocalized(sweepAnswer));
      }
    } catch (e) {
      console.warn('[succession] could not discharge the owed sweep:', e);
    }
    // The order and the failure arm live in `$lib/succession-kit`, where they
    // are unit-tested; this supplies the effects. `recoveryBusy` is raised only
    // once the claim is won, so an ordinary sign-in never paints a spinner over
    // a section that is doing nothing.
    await dischargeOwedKit({
      claim: async () => {
        const owed = await claimOwedSuccessionKit(id.actorId);
        console.info(
          `[succession] t=${Math.round(performance.now())}ms owed-kit claim for`,
          id.actorId, '->', owed,
        );
        return owed;
      },
      mint: () => {
        recoveryError = recoveryErrorGuard.write(recoveryError, '');
        recoveryBusy = true;
        console.info(`[succession] t=${Math.round(performance.now())}ms minting the successor kit`);
        return createRecoveryKit(id.secretHex, qualifiedRecoveryHandle(id));
      },
      show: (minted) => {
        applyRecoveryMinted(minted);
        // Both halves, because the display renders on `recoveryMinted && recoveryQr`
        // and a null QR would hide a kit that was otherwise perfectly minted.
        console.info(
          `[succession] t=${Math.round(performance.now())}ms successor kit minted;`,
          'secret=', !!recoveryMinted, 'qr=', !!recoveryQr,
        );
      },
      rearm: () => rearmOwedSuccessionKit(id.actorId),
      onError: (message) => {
        console.warn('[succession] the successor kit mint FAILED; re-arming:', message);
        recoveryBusy = false;
        recoveryError = recoveryErrorGuard.write(recoveryError, message);
      },
    });
  }

  /** Mint the first RecoveryKey registration (`recovery-kit-create-button`). */
  async function doCreateRecoveryKit() {
    const id = $identity;
    if (!id) return;
    recoveryError = recoveryErrorGuard.write(recoveryError, '');
    recoveryBusy = true;
    try {
      applyRecoveryMinted(await createRecoveryKit(id.secretHex, qualifiedRecoveryHandle(id)));
    } catch (e) {
      recoveryBusy = false;
      recoveryError = recoveryErrorGuard.write(recoveryError, e instanceof Error ? e.message : String(e));
    }
  }

  /** Replace the registered kit using the one the user holds
   *  (`recovery-kit-replace-button`) — the phrase check runs BEFORE the
   *  server round-trip, matching tui/linux (an empty field is a refusal the
   *  user must see immediately, not lose to a race with the nest reply). */
  async function doReplaceRecoveryKit() {
    const id = $identity;
    if (!id) return;
    const phrase = recoveryPhrase.trim();
    if (!phrase) {
      recoveryError = recoveryErrorGuard.write(recoveryError, t.settings.recovery_kit.kit_phrase_required);
      return;
    }
    recoveryError = recoveryErrorGuard.write(recoveryError, '');
    recoveryBusy = true;
    try {
      applyRecoveryMinted(
        await replaceRecoveryKit(id.secretHex, qualifiedRecoveryHandle(id), phrase),
      );
    } catch (e) {
      recoveryBusy = false;
      recoveryError = recoveryErrorGuard.write(recoveryError, e instanceof Error ? e.message : String(e));
    }
  }

  /** Open a seed-alone replacement window (`recovery-kit-lost-button`). */
  async function doRequestRecoveryKitLost() {
    const id = $identity;
    if (!id) return;
    recoveryError = recoveryErrorGuard.write(recoveryError, '');
    recoveryBusy = true;
    try {
      applyRecoveryMinted(await requestRecoveryKitLost(id.secretHex, qualifiedRecoveryHandle(id)));
    } catch (e) {
      recoveryBusy = false;
      recoveryError = recoveryErrorGuard.write(recoveryError, e instanceof Error ? e.message : String(e));
    }
  }

  /** The two kit-in-hand repairs — `recovery-pending-veto-button` and
   *  `recovery-kit-escrow-reseal-button`. Nothing is minted, so the re-read
   *  status IS the receipt (the pressed button stops rendering); the pasted kit
   *  has done its job and does not linger (tui's fold). An empty field is
   *  refused in words, like replace. */
  async function doKitInHandRepair(
    run: (secretHex: string, phrase: string) => Promise<RecoveryStatus>,
    failure: (message: string) => string,
  ) {
    const id = $identity;
    if (!id) return;
    const phrase = recoveryPhrase.trim();
    if (!phrase) {
      recoveryError = recoveryErrorGuard.write(recoveryError, t.settings.recovery_kit.kit_phrase_required);
      return;
    }
    recoveryError = recoveryErrorGuard.write(recoveryError, '');
    recoveryBusy = true;
    try {
      recoveryStatus = await run(id.secretHex, phrase);
      recoveryPhrase = '';
    } catch (e) {
      recoveryError = recoveryErrorGuard.write(
        recoveryError,
        failure(e instanceof Error ? e.message : String(e)),
      );
    } finally {
      recoveryBusy = false;
    }
  }

  /** Finish a sweep the ceremony did not (`recovery-kit-sweep-retry-button`).
   *
   *  ⚠ **The answer is the product here, not a side effect.** The button renders
   *  on unfinished work and deliberately NOT on whether this device can retry
   *  (`settings.md` § Recovery kit → *Finishing an unfinished group sweep*), so
   *  a press that cannot sweep must still say something — and on web no press
   *  ever sweeps: the retired identity's MLS state rests in the nest replica
   *  behind bearers the succession revoked. The sentence is the shared
   *  projection's, never composed here, and it lands on `error-message` like
   *  every other answer this section gives. */
  async function doRetrySuccessionSweep() {
    const id = $identity;
    if (!id) return;
    recoveryError = recoveryErrorGuard.write(recoveryError, '');
    try {
      recoveryError = recoveryErrorGuard.write(recoveryError, resolveLocalized(await successionSweepRetry(id.secretHex)));
    } catch (e) {
      recoveryError = recoveryErrorGuard.write(recoveryError, e instanceof Error ? e.message : String(e));
    }
  }

  /** Take the account back from a stolen secret, using the kit in hand
   *  (`identity-stolen-button`) — the irreversible ceremony
   *  (`identity-succession.md` § The succession statement). tui is the
   *  reference (`apps/fauna-tui/src/settings/mod.rs`, `Op::RecoveryStolen` and
   *  the fold behind it); the whole orchestration is in Rust
   *  (`libs/fauna-wasm/src/succession.rs`, web's twin of the native
   *  `fauna_client_recovery::ceremony` module), so this handler only collects,
   *  refuses, and decides what to do with the outcome.
   *
   *  The phrase check runs BEFORE the round trip, matching every other
   *  kit-in-hand ceremony here: an empty field is a refusal the user must see
   *  immediately, not lose to a race with the nest reply. */
  async function doSucceedIdentity() {
    const id = $identity;
    if (!id) return;
    const phrase = recoveryPhrase.trim();
    if (!phrase) {
      recoveryError = recoveryErrorGuard.write(recoveryError, t.settings.recovery_kit.kit_phrase_required);
      return;
    }
    recoveryError = recoveryErrorGuard.write(recoveryError, '');
    recoveryBusy = true;
    // From here the ceremony owns any supersession this session meets: it is
    // what supersedes the identity (`$lib/own-supersession-hold`).
    ownSupersessionHold.ceremonyStarted();
    try {
      // The LIVE manager only — `conversationsManagerIfReady()` never builds
      // one. A freshly built engine has no restored state, so sweeping it would
      // report a sweep that moved nothing; `no-engine` is the honest arm.
      const outcome = await succeedIdentityWithHeldKit(
        id.secretHex,
        phrase,
        conversationsManagerIfReady(),
      );
      const landed = outcome.landed;
      if (!landed) {
        // Every arm but `landed` is the shared sentence, painted verbatim and
        // wrapped in nothing (`settings.md` § Recovery kit → *The ceremony's
        // outcome is headlined by its arm*) — only nothing-moved reads as a
        // failure, and its sentence already says so.
        recoveryBusy = false;
        const message = resolveLocalized(outcome.message);
        // The undecided arm whose save was not verified carries the seed's only
        // copy: parked exactly as the persist-failure message below is.
        recoveryError = outcome.carriesTheOnlySeed
          ? recoveryErrorGuard.park(message)
          : recoveryErrorGuard.write(recoveryError, message);
        endStolenCeremony(outcome.carriesTheOnlySeed);
        return;
      }
      // Both secrets die here: the pasted kit outranks the seed, and the
      // successor seed is in the account store now (mirrors tui's fold).
      recoveryPhrase = '';
      stolenConfirm = '';
      if (!landed.persisted) {
        // The succession LANDED even though the save did not, so this must
        // never read as "nothing happened": the account is the successor's now,
        // and the seed in the message is the only way back to it. Deliberately
        // NO switch — tearing this session down would take the secret with it.
        //
        // It rides `error-message`, not the minted-kit display trio: those
        // three elements are the RecoveryKey's, and a successor seed is an
        // identity secret, not a kit. Same choice tui's fold makes.
        recoveryBusy = false;
        // The ONE write to this slot that bypasses `recoveryErrorGuard.write()`:
        // this message IS the thing becoming pending, and the guard would
        // refuse its own display.
        recoveryError = recoveryErrorGuard.park(
          t.settings.recovery_kit.stolen_persist_failed({ secret: landed.secretHex }),
        );
        endStolenCeremony(true);
        return;
      }
      // The account is the successor's: re-launch as it. `false` because the
      // row this ceremony just wrote is not confirm-flagged (a fresh
      // `add_account` entry never is), so the plain switch is the correct one —
      // the re-auth gate has exactly one call site and it is not this.
      await performSwitch(landed.newActorId, false);
      // The switch is itself the full relaunch, so a held-back supersession
      // escalation is spent, not performed. (A refused switch leaves the dead
      // session in place; with nothing held, its next refusal escalates.)
      ownSupersessionHold.adopted();
    } catch (e) {
      recoveryBusy = false;
      recoveryError = recoveryErrorGuard.write(recoveryError, e instanceof Error ? e.message : String(e));
      endStolenCeremony(false);
    }
  }

  /** The stolen-identity ceremony ended without adopting a successor. A
   *  supersession it caused and that was held back is performed now only when
   *  the user is off Account with nothing parked; otherwise the nav edge away
   *  from Account performs it (`leaveAccount`). `parked`: this ending parked
   *  the message carrying the seed's only copy. */
  function endStolenCeremony(parked: boolean) {
    if (ownSupersessionHold.ceremonyEnded({ onAccount: current === 'account', parked })) {
      performHeldBackSupersession();
    }
  }

  /** `recovery-kit-secret-copy-btn` — the `fauna://recovery` URI, not the
   *  bare hex on screen (settings.md § Recovery kit, ruling 2026-08-02): it
   *  carries the actor id and the host-qualified handle, which is what lets a
   *  later restore find the home nest with nothing typed. */
  async function copyRecoveryKitSecret() {
    if (recoveryMinted) {
      await navigator.clipboard.writeText(recoveryMinted.uri);
    }
  }

  // Inbox mode state.
  //
  // `null` — NOT a mode — until `getInboxMode` has answered, and back to `null`
  // if it fails. `settings.md` § Privacy sub-page: "The selector shows the
  // account's stored mode, never a default … Until `inbox_mode_get` has
  // answered, the mode is *unknown*". This used to open at `'allow_knock'`,
  // which marked a radio and so told every account a policy it may not hold,
  // before a single request to check — and, because the guess is replaced a
  // round trip later, no post-fetch assertion could see it. tui and linux carry
  // the same shape as `Option<String>`/`None`; apple as `String?`/`nil`.
  let inboxMode = $state<string | null>(null);
  let inboxModeError = $state('');
  let inboxModeLoading = $state(false);

  // The wire tokens come from `fauna_core::data::InboxMode::to_wire` (over
  // wasm) so a fifth mode can't drift here unnoticed the way it could when
  // tui and linux each kept their own hand-typed copy; only the i18n label
  // stays per-app. Throws if the wasm token set ever outgrows this map,
  // matching this file's other wire→label maps.
  // Populated in onMount, after ensureWasm() — NOT a top-level const, because
  // this component's <script> can run before the wasm module finishes
  // initializing (root layout's ensureLogging() is fire-and-forget, so a
  // fresh full-page reload landing directly on this route races it; a
  // top-level wasm() call here hung reached_authenticated_app on exactly
  // that path, e2e test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch).
  const inboxModeLabels: Record<string, { label: string; desc: string }> = {
    open: { label: t.status.inbox_privacy.open, desc: t.status.inbox_privacy.open_desc },
    allow_knock: { label: t.status.inbox_privacy.allow_knock, desc: t.status.inbox_privacy.allow_knock_desc },
    contacts_only: { label: t.status.inbox_privacy.contacts_only, desc: t.status.inbox_privacy.contacts_only_desc },
    closed: { label: t.status.inbox_privacy.closed, desc: t.status.inbox_privacy.closed_desc },
  };
  let inboxModes = $state<{ value: string; label: string; desc: string }[]>([]);

  // MLS key package state
  let keyPackageCount = $state<number | null>(null);
  let keyPackageError = $state('');
  let publishingKeys = $state(false);

  // Data export state
  let exporting = $state(false);
  let exportError = $state('');

  // Email filter state
  let emailFilters = $state<EmailFilter[]>([]);
  let emailFilterError = $state('');
  // The post-succession review mark, row 274 (succession-aftermath.md §
  // Adjudicating what the aftermath carries across, the fourth plane) — ids
  // of filters still awaiting the owner's verdict, cached the same way
  // linux's `Ctx.filter_marks` is: a failed re-read leaves this alone rather
  // than blanking it, so a transport blip never silently hides a mark the
  // owner has not answered.
  let filterMarks = $state<number[]>([]);
  // The Account section's inherited-filters line. Not a leg status like the
  // aftermath's lines: there is no "done" line, because the honest done state
  // of a review backlog is an absent line (mirrors tui's
  // `inherited_filters_elements`). Counted off the same cached marks the
  // filter list paints, so the line and the per-rule marks never disagree.
  let recoveryInheritedFiltersOpen = $derived(filterMarks.length);
  async function refreshFilterMarks() {
    const id = $identity;
    if (!id) return;
    try {
      filterMarks = await filterMarksList(id.secretHex);
    } catch { /* a failed re-read leaves the cache alone (see filterMarks' own doc) */ }
  }
  // The post-store-ready pass settled: the raise has had its turn on the
  // succession ledger, so re-read now rather than at the next visit (linux's
  // and apple's sinks, same hook).
  $effect(() => {
    if ($configStageSettled > 0) void refreshFilterMarks();
  });
  let showFilterForm = $state(false);
  let newFilterName = $state('');
  let newFilterRuleType = $state('SenderIs');
  let newFilterRuleValue = $state('');
  // The whole action-inputs object (kind, reject reason, Forward destination
  // and copy mode), so an edit writes back what the form has no field for.
  let newFilterAction = $state<FilterActionInputs>(defaultFilterActionInputs());
  let creatingFilter = $state(false);
  // The shared form above doubles as the edit form: null = create mode,
  // else the id of the filter being edited (create-filter's counterpart,
  // save-filter, submits in that mode).
  let editingFilterId = $state<number | null>(null);

  // Push notification state — the install's stored opt-in bit (written only
  // from the store, never from the click, so the toggle is evidence of it).
  let pushSubscribed = $state(pushOptedIn());
  let pushLoading = $state(false);
  let pushError = $state('');

  // Admin state
  let isAdmin = $state(false);

  // Moderation queue (moderation.md § Layout & flow): the union of the server-issued
  // `fauna.moderation.actions` obligation rows (which carry the enforcement `action`)
  // and this session's post-decrypt local detections (no action — the only
  // social-content signal in encrypted mode, where the nest holds ciphertext and
  // classifies nothing). The dedupe/sort rule is NOT hand-rolled here: `moderationQueue`
  // is the wasm face of the shared `fauna_client_moderation::merge_queue` the natives
  // use, so the union cannot drift per-app (priority #2). Both halves are read in
  // loadAll; the merge re-derives when either changes.
  let serverModerationActions = $state<ObligationAction[]>([]);
  let localDetections = $state<LocalDetection[]>([]);
  const moderationRows = $derived.by<QueueRow[]>(() =>
    moderationQueue(serverModerationActions, localDetections),
  );

  // The reporter's own ledger (`moderation-reports-section`; moderation.md
  // § User-initiated reporting → What the reporter is told), read beside the
  // queue on every Moderation entry. Every word is shared Rust's
  // (`ledger_row_view`, `reportLedgerWords`, `reportWithdrawVerdict`); the empty
  // line paints only off `reportLedgerLoaded`, so a read in flight never reads
  // as "you have not reported anything".
  let reportLedger = $state<ReportLedgerRow[]>([]);
  let reportLedgerLoaded = $state(false);
  let reportWithdrawStatus = $state('');
  const ledgerWords = $derived(reportLedgerWords());

  async function loadReportLedger(secretHex: string): Promise<void> {
    try {
      reportLedger = await moderationAbuseReportMine(secretHex);
      reportLedgerLoaded = true;
    } catch { /* ledger not available — the empty line stays off */ }
  }

  async function withdrawReport(row: ReportLedgerRow): Promise<void> {
    const id = $identity;
    if (!id) return;
    let failure: string | null = null;
    try {
      await moderationAbuseReportWithdraw(id.secretHex, row.report_id);
    } catch (e) {
      failure = e instanceof Error ? e.message : String(e);
    }
    const verdict = reportWithdrawVerdict(failure);
    reportWithdrawStatus = verdict ? resolveLocalized(verdict) : '';
    await loadReportLedger(id.secretHex);
  }

  // One ledger row's line — reason · status · subject · outcome — destination(s).
  function ledgerLine(row: ReportLedgerRow): string {
    const subject =
      row.subject.kind === 'post' ? row.subject.cid
      : row.subject.kind === 'message' ? row.subject.record_cid
      : row.subject.actor_id;
    const parts = [resolveLocalized(row.reason), resolveLocalized(row.status), shortId(subject)];
    if (row.outcome) parts.push(resolveLocalized(row.outcome));
    return `${parts.join(' · ')} — ${resolveLocalized(row.routed_to)}`;
  }

  // Re-read the local half whenever the receive loop pushes a new conversations
  // snapshot. The classify hook runs inside the manager during decrypt, so a spam
  // message arriving WHILE this page is open produces a detection with no page
  // event of its own — without this the queue would only ever show what existed at
  // mount (`loadAll`), and a user sitting on Settings would never see the row. The
  // poll is app-level (`+layout.svelte` `startReceivePoll`), so it ticks here too.
  $effect(() => {
    void $conversationsSnapshot; // dependency: re-run on each ingest
    localDetections = moderationLocalDetections();
  });

  // Logs sub-page (observability.md § Surfaces): the process-global `fauna_log`
  // ring, read synchronously. Reloaded each time the Logs sub-page is shown so
  // events captured since the last view appear; Clear wipes the in-memory ring
  // (the on-disk file is web-absent — § Persistence & privacy — Web). The boot
  // install (routes/+layout.svelte) seeds a startup line, so it is never empty.
  let logEntries = $state<LogEntry[]>([]);

  async function refreshLogs() {
    // Await the memoized boot logging install (ensureLogging = ensureWasm +
    // installLogging) so the read never races the async install. With the
    // promise-memoized ensureWasm there is a single wasm instance, so this page
    // reads the SAME ring the boot install seeded its startup line into — the
    // page is never blank, and no on-open re-seed workaround is needed.
    // observability.md § Surfaces / § Persistence & privacy — Web.
    await ensureLogging();
    logEntries = logSnapshot();
  }
  async function clearLogs() {
    // Clear the in-memory ring + re-read WITHOUT re-seeding, so the live view
    // drops to empty (the on-disk file is web-absent; nothing else to clear).
    logClear();
    logEntries = logSnapshot();
  }
  $effect(() => {
    if (current === 'logs') void refreshLogs();
  });
  $effect(() => {
    if (current === 'account') void loadAccounts();
  });

  // Muted words sub-page (moderation.md § Muted keywords): the sealed,
  // user-global `fauna.state.moderation` muted-keyword list, CRUD over the
  // wasm `mutedKeywords{List,Set}` seam (rpc.ts). Reloaded on entry so a
  // return visit (e.g. after the conversation collapse read the cache) sees
  // the current stored list; both calls return the shared page record so
  // add/remove re-render exactly what was saved without a re-fetch.
  //
  // Held as the whole record — `{ words, loaded }` — rather than a bare array:
  // `loaded` is what separates "you have muted nothing" from "we have not read
  // your list yet", and an app that re-derives that itself gets it wrong (all 7
  // did). `docs/goal/ui/README.md` § *List pages: loading is not empty*. A
  // failed read replaces nothing, so the page stays unloaded and `error-message`
  // does the talking.
  let mutedWords = $state<MutedWordsSnapshot>({ keywords: [], loaded: false });
  let newMutedWord = $state('');
  let mutedWordsError = $state('');
  // Bumped by every gesture: a store-change re-read that was in flight when
  // the user added or removed a term is dropped, so it never paints an older
  // list over the gesture's own answer.
  let mutedWordsGesture = 0;

  async function loadMutedWords() {
    const id = $identity;
    if (!id) return;
    try {
      mutedWords = await mutedKeywordsList(id.secretHex);
    } catch (e) {
      mutedWordsError = e instanceof Error ? e.message : String(e);
    }
  }

  // The open page's re-read on a store-change notice (`$lib/store-change`):
  // the same read, level-shaped — it replaces only the list (the draft in the
  // input is its own state), and a failed or overtaken re-read replaces nothing.
  async function rereadMutedWords() {
    const id = $identity;
    if (!id) return;
    const gesture = mutedWordsGesture;
    try {
      const snapshot = await mutedKeywordsList(id.secretHex);
      if (gesture === mutedWordsGesture) mutedWords = snapshot;
    } catch {
      // The list on screen stands; the next notice or visit reads again.
    }
  }

  // Both gestures send a DELTA, never the page's list wholesale (apps row
  // 214): the shared seam re-reads the stored list inside its own CAS update,
  // so a term another device stored since this page loaded survives the click.
  async function addMutedWord() {
    const id = $identity;
    const term = newMutedWord.trim();
    if (!id || !term) return;
    mutedWordsGesture += 1;
    try {
      mutedWords = await mutedKeywordsAdd(id.secretHex, term);
      newMutedWord = '';
    } catch (e) {
      mutedWordsError = e instanceof Error ? e.message : String(e);
    }
  }

  async function removeMutedWord(term: string) {
    const id = $identity;
    if (!id) return;
    mutedWordsGesture += 1;
    try {
      mutedWords = await mutedKeywordsRemove(id.secretHex, term);
    } catch (e) {
      mutedWordsError = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    if (current !== 'muted-words') return;
    void loadMutedWords();
    return onStoreChange(() => void rereadMutedWords());
  });

  // ── Unattested-member review — the permanent
  // Settings sub-page's state. `loaded` separates "nothing open" from "not
  // read yet" (README.md § List pages: loading is not empty), same rule as
  // `mutedWords` above.
  interface MemberReviewRowVM {
    review: MemberReview;
    text: MemberReviewRowText;
  }
  let memberReviewRows = $state<MemberReviewRowVM[]>([]);
  let memberReviewLoaded = $state(false);
  let memberReviewError = $state('');

  /** The ephemeral kit-side pass's render gate — `succession-aftermath.md`
   *  § Propagation, item (ii): render only where a sweep ran THIS session,
   *  never on an ordinary sign-in that merely happens to carry open items
   *  (that is what the permanent page below is for). Loaded alongside the
   *  roster (`loadMemberReview`) rather than a second effect, since both are
   *  read together and the pass has nothing to show without either. */
  let ephemeralReviewActive = $state(false);

  /** The last succession's group sweep, if one ran in this tab — the
   *  ceremony's own outcome, rendered in the flow that ran it, as the shared
   *  projection selected it (`settings.md` § Recovery kit → *The sweep's own
   *  lines*). web's twin of tui's `App::succession_sweep`; `null` on every
   *  ordinary sign-in. Read beside the review-pass witness, since the two
   *  describe the same ceremony and the pass renders directly under these
   *  lines. */
  let sweepCopy = $state<SweepCopy | null>(null);

  /** Re-read the roster: a verdict another device recorded must not be
   *  re-asked here, which is why every mutation below reloads rather than
   *  patching the cached list in place. Resolves each row's handle BEFORE
   *  building its text — `memberReviewHandleForPerson` reads live
   *  membership, so it must run before any Remove for the same person. */
  async function loadMemberReview() {
    const id = $identity;
    if (!id) return;
    try {
      const reviews = await memberReviewList();
      memberReviewRows = await Promise.all(
        reviews.map(async (review) => {
          const handle = await memberReviewHandleForPerson(review.person);
          const text = await memberReviewRowText(review.person, review.reasons, handle);
          return { review, text };
        }),
      );
      memberReviewLoaded = true;
    } catch (e) {
      memberReviewError = e instanceof Error ? e.message : String(e);
    }
    // No round trip (a synchronous local read on the wasm side) — never
    // allowed to leave `ephemeralReviewActive` stuck true from a stale sign-in
    // if it throws, so this sits outside the `try` above.
    try {
      ephemeralReviewActive = await ephemeralReviewPassActive(id.secretHex);
    } catch (e) {
      console.warn('[settings] ephemeral review pass witness read failed:', e);
      ephemeralReviewActive = false;
    }
    // Same shape, same reason: a local read that must never leave a stale
    // sweep painted for the wrong identity if it throws.
    try {
      sweepCopy = await successionSweepCopy(id.secretHex);
    } catch (e) {
      console.warn('[settings] succession sweep read failed:', e);
      sweepCopy = null;
    }
  }

  /** *Review The Rest Later* — hides the ephemeral pass and decides nothing:
   *  the open items stay exactly where they are, inherited by the permanent
   *  review page below. Clears the witness first so a failed follow-up read
   *  cannot leave the pass stuck visible. */
  async function deferMemberReviewPass() {
    const id = $identity;
    if (!id) return;
    ephemeralReviewActive = false;
    try {
      await deferEphemeralReviewPass(id.secretHex);
    } catch (e) {
      console.warn('[settings] deferring the ephemeral review pass failed:', e);
    }
  }

  /** Record **Keep** — closes every open item for `person` with no group
   *  changes; a concurrent device may have already answered, which is a
   *  success no-op, never an error. Re-reads after (the answered row drops
   *  out). */
  async function keepMemberReview(personHex: string) {
    const id = $identity;
    if (!id) return;
    try {
      await memberReviewKeep(personHex);
      memberReviewError = '';
      await loadMemberReview();
    } catch (e) {
      memberReviewError = e instanceof Error ? e.message : String(e);
    }
  }

  /** `CrossGroupEviction` → this app's `error-message` text — `''` when the
   *  row is expected to drop out cleanly on the next re-read. The wasm seam
   *  only invokes the verdict-persisting write once an eviction is COMPLETE
   *  (`failed`/`unreachable` both empty), so a partial eviction here never
   *  means a failed persist — only a seat still standing. Mirrors linux
   *  `member_review.rs::remove_result_message` / android's
   *  `removeResultMessage`. */
  function memberReviewRemoveMessage(eviction: CrossGroupEviction, who: string): string {
    if (eviction.failed.length === 0 && eviction.unreachable.length === 0) return '';
    const parts: string[] = [];
    if (eviction.failed.length > 0) {
      const groups = eviction.evicted.length + eviction.failed.length;
      parts.push(
        t.settings.recovery_kit.review_remove_partial({
          who,
          removed: String(eviction.evicted.length),
          groups: String(groups),
        }),
      );
    } else if (eviction.evicted.length > 0) {
      parts.push(
        t.settings.recovery_kit.review_remove_done_here({
          who,
          removed: String(eviction.evicted.length),
        }),
      );
    } else {
      parts.push(t.settings.recovery_kit.review_remove_none_here({ who }));
    }
    const folders = eviction.unreachable.filter((s) => s.class === 'FolderChannel').length;
    if (folders > 0) {
      parts.push(t.settings.recovery_kit.review_remove_folder_seats({ seats: String(folders) }));
    }
    const unsynced = eviction.unreachable.filter((s) => s.class === 'ChatGroupNoThreadHere').length;
    if (unsynced > 0) {
      parts.push(t.settings.recovery_kit.review_remove_unsynced_seats({ seats: String(unsynced) }));
    }
    return parts.join(' ');
  }

  /** Record **Remove** — evicts `person` from every group of the owner's
   *  they are in NOW (re-derived, never from the stored item; the wasm seam
   *  persists only what the eviction earned, so this app cannot record
   *  `Removed` from its own reasoning). `who` is resolved from the cached
   *  row BEFORE the call — there is no seat left to read a handle off
   *  afterward. Re-reads the roster regardless: a full eviction drops the
   *  row, a partial one re-renders from the unchanged state. */
  async function removeMemberReview(personHex: string) {
    const id = $identity;
    if (!id) return;
    const row = memberReviewRows.find((r) => r.review.person === personHex);
    const who = row ? resolveLocalized(row.text.who) : t.settings.recovery_kit.review_unknown_person;
    try {
      const eviction = await memberReviewRemove(personHex);
      memberReviewError = memberReviewRemoveMessage(eviction, who);
      await loadMemberReview();
    } catch (e) {
      memberReviewError = e instanceof Error ? e.message : String(e);
    }
  }

  $effect(() => {
    // 'account' hosts the Recovery kit section (the ephemeral pass);
    // 'member-review' is the permanent page — both render the SAME roster
    // (`succession-aftermath.md` § Propagation: one family, two surfaces).
    if (current === 'member-review' || current === 'account') void loadMemberReview();
  });

  // Spam preferences state
  let spamThreshold = $state(0.5);
  let phishingThreshold = $state(0.3);
  let spamPrefsLoading = $state(false);
  let spamPrefsSaved = $state(false);
  let spamPrefsError = $state('');

  onMount(async () => {
    await ensureWasm();
    inboxModes = inboxModeValues().map((value) => {
      const known = inboxModeLabels[value];
      if (!known) throw new Error(`inbox mode "${value}" has no web label`);
      return { value, ...known };
    });
    identity.init();

    if (!$identity) {
      goto('/app/onboarding', { replaceState: true });
      return;
    }

    loading = false;
    loadAll();

    if ($identity?.secretHex) {
      isAdmin = await checkIsAdmin($identity.secretHex);
    }
  });

  async function loadAll() {
    const id = $identity;
    if (!id) return;
    try {
      quota = await fetchQuota(id.secretHex);
    } catch { /* quota not available */ }
    try {
      features = await fetchFeatures(id.secretHex);
    } catch { /* feature limits not available */ }
    try {
      inboxMode = await getInboxMode(id.secretHex);
    } catch { /* inbox mode not available */ }
    try {
      keyPackageCount = await wsKeypackageCount(id.secretHex, id.actorId);
      if (keyPackageCount < 5) {
        await publishMlsKeys(id.secretHex, id.actorId);
      }
    } catch { /* MLS key packages not available */ }
    try {
      emailFilters = await listEmailFilters(id.secretHex);
    } catch { /* filters not available */ }
    try {
      filterMarks = await filterMarksList(id.secretHex);
    } catch { /* a failed re-read leaves the cache alone (see filterMarks' own doc) */ }
    try {
      const prefs = await getSpamPreferences(id.secretHex);
      spamThreshold = prefs.spam_threshold;
      phishingThreshold = prefs.phishing_threshold;
    } catch { /* spam prefs not available */ }
    try {
      serverModerationActions = await moderationActions(id.secretHex);
    } catch { /* moderation actions not available */ }
    await loadReportLedger(id.secretHex);
    // (The queue's local half is not read here — the `$effect` above keeps it in
    // sync with every receive-loop ingest, mount included.)
  }

  async function publishMlsKeys(secretHex: string, actorId: string) {
    publishingKeys = true;
    keyPackageError = '';
    try {
      // Mint + publish through the ONE conversations engine (the same
      // `ensureKeypackages` surface linux/windows drive from their settings /
      // login paths) — never a second standalone engine, whose private init
      // keys the Welcome-processing engine could not see (devices.md
      // § Cross-device MLS group-state sync, slice 6). `scheduleMlsSave` rides
      // the manager's replica autosave so the fresh init keys survive a reload
      // (the web engine is in-memory; the nest replica is its persistence).
      const m = await getConversationsManager();
      await m.ensureKeypackages(KEYPACKAGE_TARGET);
      await m.ensureLastResortKeypackage();
      scheduleMlsSave();
      keyPackageCount = await wsKeypackageCount(secretHex, actorId);
    } catch (e) {
      keyPackageError = rejectionText(e, t.settings.errors.publish_keys);
    } finally {
      publishingKeys = false;
    }
  }

  async function updateInboxMode(newMode: string) {
    const id = $identity;
    if (!id || inboxModeLoading) return;
    inboxModeLoading = true;
    inboxModeError = '';
    try {
      await setInboxMode(id.secretHex, newMode);
      inboxMode = newMode;
    } catch (e) {
      inboxModeError = rejectionText(e, t.settings.errors.update_inbox);
    } finally {
      inboxModeLoading = false;
    }
  }

  async function doChangeHandle() {
    const id = $identity;
    const candidate = newHandle.trim();
    if (!id || !candidate) return;
    // Client-side format check first — the SAME shared validator the nest
    // enforces (linux/native validate natively/via UniFFI; web via the wasm
    // wrapper). A taken handle stays server-authoritative (surfaced below).
    const formatError = validateHandle(candidate);
    if (formatError) {
      changeHandleError = formatError;
      return;
    }
    changingHandle = true;
    changeHandleError = '';
    try {
      // The reply's echoed `handle` is NOT applied yet — it names a queued,
      // cancellable pending action (settings.md § Pending actions), never a
      // local cache. Feeding it into `identity.completeRegistration` here
      // would show a handle the user does not own until `execute_after` (or
      // never, if they cancel) — the same trap tui/linux's reference shape
      // deliberately avoids. The scheduled change is surfaced by the
      // pending-actions section instead, refreshed below.
      await changeHandle(id.secretHex, candidate);
      newHandle = '';
      await loadPendingActions();
    } catch (e) {
      changeHandleError = rejectionText(e, t.settings.errors.change_handle);
    } finally {
      changingHandle = false;
    }
  }

  // Only SCHEDULES a 14-day cancellable pending action (settings.md § User
  // actions, ruled 2026-08-26: "the doc is right, the teardown apps are
  // wrong") — no sign-out, no credential/store erase, no navigation here.
  // `account-scoping.md` § Erasure follows scope binds erasure at
  // EXECUTION, never at request time. The re-listed pending-actions row is
  // the receipt; its cancel button is the way back within the window.
  async function doDeleteAccount() {
    const id = $identity;
    if (!id) return;
    try {
      await deleteAccount(id.secretHex);
      deleteAccountError = '';
      deleteConfirmText = '';
      await loadPendingActions();
    } catch (e) {
      deleteAccountError = rejectionText(e, t.settings.errors.delete_account);
    }
  }

  async function exportData() {
    const id = $identity;
    if (!id) return;
    exporting = true;
    exportError = '';
    try {
      const headers = await authHeaders(id.secretHex);
      // `include_blobs=true` is what makes this the whole archive rather than
      // an index of it — the nest defaults the flag off. Not a user choice:
      // account-data-plane.md § Nest-side requirements item 1, Payload stores
      // decision (5).
      const res = await fetch(nodeUrl() + '/api/v1/export?include_blobs=true', { headers });
      if (!res.ok) throw new Error(t.settings.errors.export_status({ status: String(res.status) }));
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      const date = new Date().toISOString().slice(0, 10);
      a.download = `fauna-export-${date}.zip`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e: any) {
      exportError = e.message || t.settings.errors.export;
    } finally {
      exporting = false;
    }
  }

  function openCreateFilterForm() {
    editingFilterId = null;
    newFilterName = '';
    newFilterRuleType = 'SenderIs';
    newFilterRuleValue = '';
    newFilterAction = defaultFilterActionInputs();
    emailFilterError = '';
    showFilterForm = true;
  }

  // Open the shared form pre-populated for an existing filter — a fresh
  // getEmailFilter (not the cached list row), so the edit reflects current
  // server state. The row's own filter-edit button is gated filterIsEditableFor,
  // so decode failure here means the filter changed server-side between
  // list-load and the click (rare) rather than the common case.
  async function handleEditFilter(filterId: number) {
    const id = $identity;
    if (!id) return;
    emailFilterError = '';
    try {
      await ensureWasm();
      const filter = await getEmailFilter(id.secretHex, filterId);
      const rule = filter.rules[0] !== undefined ? describeEmailFilterRule(filter.rules[0] as any) : null;
      const action = describeEmailFilterActionInputs(filter.action as any);
      if (!rule || !action) {
        emailFilterError = t.settings.errors.update_filter;
        return;
      }
      editingFilterId = filter.id;
      newFilterName = filter.name;
      newFilterRuleType = rule[0];
      newFilterRuleValue = rule[1];
      newFilterAction = action;
      showFilterForm = true;
    } catch (e: any) {
      emailFilterError = (e instanceof Error ? e.message : String(e)) || t.settings.errors.update_filter;
    }
  }

  async function handleSaveFilter() {
    const id = $identity;
    if (!id || editingFilterId === null) return;
    creatingFilter = true;
    emailFilterError = '';
    try {
      await ensureWasm();
      const rule = encodeEmailFilterRule(newFilterRuleType, newFilterRuleValue);
      const action = encodeEmailFilterActionInputs(newFilterAction);
      await updateEmailFilter(id.secretHex, editingFilterId, newFilterName, [rule], 'all', action, 0);
      emailFilters = await listEmailFilters(id.secretHex);
      showFilterForm = false;
      editingFilterId = null;
      newFilterName = '';
      newFilterRuleValue = '';
    } catch (e: any) {
      emailFilterError = (e instanceof Error ? e.message : String(e)) || t.settings.errors.update_filter;
    } finally {
      creatingFilter = false;
    }
  }

  async function handleCreateFilter() {
    const id = $identity;
    if (!id) return;
    creatingFilter = true;
    emailFilterError = '';
    try {
      // Shared encoder (fauna_protocol::email) — the (kind, value) / (action,
      // reason) → typed-wire map, instead of a per-app hand-rolled switch
      // that could drift from what the nest deserializes. Throws on an unknown
      // kind (caught below). An empty reject reason rides the canonical default.
      await ensureWasm();
      const rule = encodeEmailFilterRule(newFilterRuleType, newFilterRuleValue);
      const action = encodeEmailFilterActionInputs(newFilterAction);
      await createEmailFilter(id.secretHex, newFilterName, [rule], 'all', action, 0);
      emailFilters = await listEmailFilters(id.secretHex);
      showFilterForm = false;
      newFilterName = '';
      newFilterRuleValue = '';
    } catch (e: any) {
      // The encoder throws a plain string on an unknown kind; createEmailFilter
      // rejects with an Error. Surface either.
      emailFilterError = (e instanceof Error ? e.message : String(e)) || t.settings.errors.create_filter;
    } finally {
      creatingFilter = false;
    }
  }

  async function handleDeleteFilter(filterId: number) {
    const id = $identity;
    if (!id) return;
    emailFilterError = '';
    // Ordered AFTER the delete, deliberately (this plane's own trap note:
    // a delete of a MARKED row must also record the verdict, but recording
    // it first and then failing to delete would silence the mark while the
    // rule kept running — the exact silent-hiding failure the surface
    // exists to prevent).
    const wasMarked = filterMarks.includes(filterId);
    try {
      await deleteEmailFilter(id.secretHex, filterId);
      emailFilters = emailFilters.filter(f => f.id !== filterId);
    } catch (e: any) {
      emailFilterError = e.message || t.settings.errors.delete_filter;
      return;
    }
    if (wasMarked) {
      try {
        await filterMarkRemoved(id.secretHex, filterId);
        filterMarks = filterMarks.filter(mid => mid !== filterId);
      } catch {
        // The delete already succeeded, the outcome that matters. Leave the
        // mark open — re-asking about a rule that no longer exists is
        // untidy, and strictly the safe direction (mirrors linux's
        // record_filter_removal, best-effort/log-only).
      }
    }
  }

  /** Record the owner's **Keep** verdict, then re-read the marks. */
  async function handleKeepFilterMark(filterId: number) {
    const id = $identity;
    if (!id) return;
    emailFilterError = '';
    try {
      await filterMarkKeep(id.secretHex, filterId);
      filterMarks = await filterMarksList(id.secretHex);
    } catch (e: any) {
      emailFilterError = e.message || t.settings.errors.keep_filter;
    }
  }

  async function saveSpamPreferences() {
    const id = $identity;
    if (!id) return;
    spamPrefsLoading = true;
    spamPrefsError = '';
    spamPrefsSaved = false;
    try {
      await updateSpamPreferences(id.secretHex, {
        spam_threshold: spamThreshold,
        phishing_threshold: phishingThreshold,
      });
      spamPrefsSaved = true;
      setTimeout(() => { spamPrefsSaved = false; }, 3000);
    } catch (e: any) {
      spamPrefsError = e.message || t.settings.errors.save_prefs;
    } finally {
      spamPrefsLoading = false;
    }
  }

  // The opt-in toggle (`settings.md` § Push notifications): on → enable, off →
  // disable, then re-read the stored bit — a failed enable leaves it off, a
  // disable clears it even when the nest is unreachable — and paint the
  // failure on the inline line.
  async function togglePushOptIn(on: boolean) {
    const id = $identity;
    if (!id) return;
    pushLoading = true;
    pushError = '';
    try {
      if (on) await enablePush(id.secretHex);
      else await disablePush(id.secretHex);
    } catch (e: any) {
      pushError = `${t.settings.push_notifications.update_failed}: ${e?.message ?? e}`;
    } finally {
      pushSubscribed = pushOptedIn();
      pushLoading = false;
    }
  }

  async function copyActorId() {
    if ($identity?.actorId) {
      await copyAndConfirm($identity.actorId);
      copiedActorId = $identity.actorId;
      actorIdCopied = true;
      setTimeout(() => { actorIdCopied = false; }, 2000);
    }
  }

  // `storedNestUrl()`, not `nodeUrl()`: what the user copies is the nest URL
  // their account is bound to — the dial seam redirects the socket, never the
  // truth (`fauna_launch_machine::dial`).
  async function copyNodeUrl() {
    const url = storedNestUrl();
    await copyAndConfirm(url);
    copiedNodeUrl = url;
    nodeUrlCopied = true;
    setTimeout(() => { nodeUrlCopied = false; }, 2000);
  }

  async function signOut() {
    // The whole sign-out refuses while another tab serves any account it would
    // erase — nothing erased, no credentials wiped, still signed in — and says
    // so on the page's `error-message`, with the confirm button still showing
    // so closing the other tab and pressing it again is the whole remedy
    // (`account-scoping.md` § Concurrent instances → *An erase refuses while a
    // sibling serves the account*). Asked before `logout()` is entered, which
    // wipes the credentials synchronously ahead of its first `await`.
    if (await signOutBlockedByAnotherTab()) {
      error = t.settings.sign_out_blocked_other_window;
      return;
    }
    // Await the erase before navigating: `logout()` clears the account registry
    // through wasm, and a `goto` that races it would leave `fauna/index` behind.
    await identity.logout();
    goto('/app/onboarding');
  }

</script>

<!-- Escape declines the Stage-2 re-auth prompt (cancel/backdrop/Escape are one
     decline path, ratified) — a pure no-op, so an unexpected keypress is safe. -->
<svelte:window onkeydown={(e) => { if (e.key === 'Escape' && reauthTarget) cancelReauth(); }} />

<h1 data-testid={IDS.PAGE_HEADING}>{pageHeadingText}</h1>

<MessageBanner bind:error />

<div data-testid={IDS.SETTINGS_VIEW}>
{#if loading}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.sign_in_required}</p>

{:else if current === ''}
  <!-- ── Status (default sub-page) — live nest data: identity / nest / quota /
       build (settings.md § Live-data placement). `account-settings-link` is the
       settings page landmark and lives here on the default sub-page (matching
       linux status.rs + the cross-app navigation smoke test). ── -->
  <section class="section" data-testid={IDS.ACCOUNT_SETTINGS_LINK}>
    <h2>{t.common.identity}</h2>
    <div class="field">
      <span class="field-label">{t.common.actor_id}</span>
      <code class="mono" data-testid={IDS.ACCOUNT_ACTOR_ID}>{$identity?.actorId}</code>
      <button class="btn-copy" data-testid={IDS.ACCOUNT_ACTOR_ID_COPY_BTN} data-copied={copiedActorId ?? undefined} onclick={copyActorId}>{actorIdCopied ? t.common.copied : t.common.copy}</button>
    </div>
    {#if $identity?.handle}
      <div class="field">
        <span class="field-label">{t.common.handle}</span>
        <span data-testid={IDS.SETTINGS_HANDLE_LABEL}>{$identity.handle}@{$identity.domain}</span>
      </div>
    {/if}
    {#if $identity?.tier}
      <div class="field">
        <span class="field-label">{t.common.tier}</span>
        <span>{$identity.tier}</span>
      </div>
    {/if}
    <div class="field">
      <span class="field-label">{t.settings.nest_url}</span>
      <input data-testid={IDS.SETTINGS_NEST_URL_FIELD} type="text" class="input" value={storedNestUrl()} readonly />
      <button class="btn-copy" data-testid={IDS.STATUS_NODE_URL_COPY_BTN} data-copied={copiedNodeUrl ?? undefined} onclick={copyNodeUrl}>{nodeUrlCopied ? t.common.copied : t.common.copy}</button>
    </div>
  </section>

  {#if quota}
    <section class="section" data-testid={IDS.QUOTA_SECTION}>
      <h2>{t.status.quota.title}</h2>
      <div data-testid={IDS.SETTINGS_STORAGE_BAR} class="storage-bar">
        <div class="storage-fill" style="width: {quotaPercent(quota.storage.used_bytes, quota.storage.max_bytes)}%"></div>
      </div>
      <p data-testid={IDS.SETTINGS_STORAGE_TEXT} class="muted storage-label">{byteSize(quota.storage.used_bytes)} / {byteSize(quota.storage.max_bytes)}</p>
      <div class="field">
        <span class="field-label">{t.common.inbox}</span>
        <span data-testid={IDS.QUOTA_INBOX}>{byteSize(quota.inbox.used_bytes)} / {byteSize(quota.inbox.max_bytes)}</span>
      </div>
      <div class="field">
        <span class="field-label">{t.common.storage}</span>
        <span data-testid={IDS.QUOTA_STORAGE}>{byteSize(quota.storage.used_bytes)} / {byteSize(quota.storage.max_bytes)}</span>
      </div>
      <div class="field">
        <span class="field-label">{t.common.devices}</span>
        <span data-testid={IDS.QUOTA_DEVICES}>{quota.devices.used} / {quota.devices.max}</span>
      </div>
    </section>
  {/if}

  <!--
    `feature-limits-section` — the gated-feature plane's transparency read
    (`dynamic-features.md` § Transparency & auditability, boundary 4: "no
    silent gates"). Placed directly after Quota as its sibling "what bounds
    me" surface (`settings.md` § Layout & flow item 2b). This block paints;
    it decides nothing — every judgement (which cells survived the tier meet,
    per-cell tier attribution, available/restricted/hidden) is
    `fetchFeatures`'s output, mirroring tui's `feature_limits_elements` and
    linux's `views::status::update_features` field-for-field.
  -->
  {#if features}
    <section class="section" data-testid={IDS.FEATURE_LIMITS_SECTION}>
      <h2>{t.features.section_title}</h2>
      {#if features.filter((r) => r.affordance !== 'hidden').length === 0}
        <p class="muted" data-testid={IDS.FEATURE_LIMITS_EMPTY}>{t.features.empty}</p>
      {:else}
        {#each features.filter((r) => r.affordance !== 'hidden') as row}
          <div class="field-group" data-testid={IDS.FEATURE_LIMITS_ROW}>
            <div class="field">
              <span class="field-label" data-testid={IDS.FEATURE_LIMITS_NAME}>{resolveLocalized(row.name)}</span>
              <span data-testid={IDS.FEATURE_LIMITS_STATUS}>{resolveLocalized(row.status)}</span>
            </div>
            {#if row.restriction}
              <p class="muted" data-testid={IDS.FEATURE_LIMITS_RESTRICTION}>{resolveLocalizedNested(row.restriction)}</p>
            {/if}
            {#each row.cells as cell}
              <div class="field" data-testid={IDS.FEATURE_LIMITS_QUOTA}>
                <span class="field-label" data-testid={IDS.FEATURE_LIMITS_QUOTA_LABEL}>{resolveLocalizedNested(cell.label)}</span>
                <span data-testid={IDS.FEATURE_LIMITS_QUOTA_VALUE}>{cellValueText(cell)}</span>
                <span class="muted" data-testid={IDS.FEATURE_LIMITS_QUOTA_TIER}>{resolveLocalized(cell.tier_label)}</span>
              </div>
            {/each}
          </div>
        {/each}
      {/if}
    </section>
  {/if}

  <!-- `settings-region-section` — the region content plane's transparency
       surface, after feature limits (linux's Status sub-page order). -->
  <RegionSettingsSection />

  {#if isAdmin}
    <section class="section">
      <h2>{t.status.admin_section.title}</h2>
      <p class="muted">{t.status.admin_section.description}</p>
      <a href="/app/admin/" class="btn">{t.status.admin_section.dashboard}</a>
    </section>
  {/if}

  <section class="section build-info">
    <h2>{t.status.build.title}</h2>
    <div class="field">
      <span class="field-label">{t.common.actor_id}</span>
      <code class="mono">{$identity?.actorId}</code>
      <button class="btn-copy" data-testid={IDS.STATUS_ACTOR_ID_COPY_BTN} data-copied={copiedActorId ?? undefined} onclick={copyActorId}>{actorIdCopied ? t.common.copied : t.common.copy}</button>
    </div>
    <div class="field">
      <span class="field-label">{t.status.build.commit}</span>
      {#if import.meta.env.VITE_GIT_SHA !== 'dev'}
        <!-- Plain text, not a link: builds are stamped with a development-repo
             commit that is not resolvable from the public mirror's history. -->
        <code class="mono" data-testid={IDS.STATUS_BUILD_SHA}>{import.meta.env.VITE_GIT_SHA.slice(0, 12)}</code>
      {:else}
        <code class="mono" data-testid={IDS.STATUS_BUILD_SHA}>dev</code>
      {/if}
    </div>
    <p class="muted small">
      {t.status.build.verify_hint}
    </p>
  </section>

{:else if current === 'account'}
  <!-- ── Multi-account switcher (long-term-store.md § Multi-account evolution):
       the identities held on this client install + switch/add/remove. The shared
       `fauna_client_accounts` registry (over wasm) owns the list + active pointer
       + migration; the row title comes from the shared
       `fauna_core::format::account_display_label` over wasm, so the empty-handle
       fallback can't drift from the linux reference (settings/account.rs). ── -->
  <section class="section">
    <h2>{t.settings.account_page.accounts}</h2>
    <div class="account-switcher" data-testid={IDS.ACCOUNT_SWITCHER_LIST}>
      {#each accounts as acct, i (acct.actor_id)}
        {@const isActive = acct.actor_id.toLowerCase() === servedActor}
        <!-- The `account-switcher-item` testid sits on the ROW (ui.yaml types it a
             view: the row hosts the scoped toggle + remove controls); the activate
             button fills the row's remaining width, so tapping "the row" is
             tapping the button. -->
        <div class="account-row" class:active={isActive} data-testid={IDS.ACCOUNT_SWITCHER_ITEM}>
          <button
            class="account-main"
            disabled={isActive || switchingAccount}
            onclick={() => requestSwitchAccount(acct.actor_id)}
          >
            <span data-testid={IDS.ACCOUNT_ITEM_HANDLE}>{accountDisplayLabel(acct.handle, acct.actor_id)}</span>
            {#if isActive}
              <span class="active-badge" data-testid={IDS.ACCOUNT_ITEM_ACTIVE_INDICATOR}>{t.common.active}</span>
            {/if}
          </button>
          <!-- Stage-2 "require re-auth to activate" flag — on EVERY row, the ACTIVE
               one included (unlike remove): the natural target is the user's admin
               identity, usually the account you are already on, and the admin
               auto-default flags exactly that row. Setting the flag never prompts;
               only *activating* a flagged account does. `data-state` mirrors the
               flag for the e2e's get_attr (the linux toggle's "state" attr twin). -->
          <label class="confirm-toggle" title={t.settings.account_page.require_confirm_toggle}>
            <input
              type="checkbox"
              data-testid={IDS.ACCOUNT_REQUIRE_CONFIRM_TOGGLE}
              data-state={acct.require_confirm_to_activate ? 'on' : 'off'}
              checked={acct.require_confirm_to_activate}
              onchange={(e) => setRequireConfirm(acct.actor_id, e.currentTarget.checked)}
            />
            <span class="confirm-toggle-label">{t.settings.account_page.require_confirm_toggle}</span>
          </label>
          {#if !isActive}
            <button
              class="btn ghost small"
              data-testid={IDS.ACCOUNT_REMOVE_BUTTON}
              onclick={() => removeAccount(acct.actor_id)}
            >{t.common.remove}</button>
          {/if}
        </div>
      {/each}
    </div>
    {#if reauthTarget}
      <!-- Stage-2 in-app re-auth confirm (`account-activate-reauth-prompt`) — the
           shape the linux slice ratified for the no-native-prompt platforms,
           mirroring the file-version-restore-confirm-modal family. A confirmation,
           not a credential check; cancel / backdrop / Escape are all the decline
           path (a pure no-op). -->
      <div class="modal-backdrop" role="presentation" onclick={cancelReauth}>
        <div
          data-testid={IDS.ACCOUNT_ACTIVATE_REAUTH_PROMPT}
          class="modal"
          role="dialog"
          aria-modal="true"
          onclick={(e) => e.stopPropagation()}
        >
          <h2>{t.settings.account_page.reauth_prompt_title}</h2>
          <p>{t.settings.account_page.reauth_prompt_body({ account: reauthTarget.label })}</p>
          <div class="modal-actions">
            <button
              data-testid={IDS.ACCOUNT_ACTIVATE_REAUTH_CANCEL_BUTTON}
              class="btn"
              onclick={cancelReauth}
            >{t.common.cancel}</button>
            <button
              data-testid={IDS.ACCOUNT_ACTIVATE_REAUTH_CONFIRM_BUTTON}
              class="btn primary"
              onclick={confirmReauth}
              disabled={switchingAccount}
            >{t.settings.account_page.reauth_confirm}</button>
          </div>
        </div>
      </div>
    {/if}
    <button class="btn" data-testid={IDS.ACCOUNT_ADD_BUTTON} onclick={addAccount} disabled={switchingAccount}>
      {t.settings.account_page.add_account}
    </button>
    {#if accountsError}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{accountsError}</p>{/if}
  </section>

  <!-- ── Account — pure actions (settings.md § Live-data placement): handle
       change / export / sign-out / delete. (The `account-settings-link` page
       landmark lives on the Status sub-page, matching linux + the nav smoke test.) ── -->
  <section class="section">
    <h2>{t.common.account}</h2>
    {#if $identity?.handle}
      <div class="field">
        <span class="field-label">{t.common.handle}</span>
        <span>{$identity.handle}@{$identity.domain}</span>
      </div>
    {/if}
    {#if !showSignOutConfirm}
      <button data-testid={IDS.SIGN_OUT_BUTTON} class="btn danger" onclick={() => showSignOutConfirm = true}>{t.settings.sign_out}</button>
    {:else}
      <div class="confirm-box">
        <p>{t.settings.sign_out_confirm}</p>
        <button data-testid={IDS.SIGN_OUT_CONFIRM_BUTTON} class="btn danger" onclick={signOut}>{t.settings.sign_out}</button>
        <button data-testid={IDS.SIGN_OUT_CANCEL_BUTTON} class="btn ghost" onclick={() => showSignOutConfirm = false}>{t.common.cancel}</button>
      </div>
    {/if}
  </section>

  <!-- Identity export (settings.md § Identity export) — the QR a second device scans to
       import this identity. Placed before Change handle, mirroring the shipped Apple order.
       The description + toggle are always visible; the warning and the QR appear ONLY after
       the user presses show. Not gated on the handle: a handle-less client writes the bare
       secret form, and the handle cache may not have hydrated yet. -->
  {#if $identity}
    <section class="section" data-testid={IDS.IDENTITY_EXPORT_SECTION}>
      <h2>{t.settings.identity_export.title}</h2>
      <p class="muted" data-testid={IDS.IDENTITY_EXPORT_DESCRIPTION}>{t.settings.identity_export.desc}</p>

      {#if identityQr}
        <p class="error" data-testid={IDS.IDENTITY_EXPORT_WARNING}>{t.settings.identity_export.warning}</p>
        <!-- One <rect> per dark module, offset by the quiet zone. The viewBox is in module
             units, so the browser scales the grid for us and the code stays crisp at any
             size (no canvas, no imperative redraw on toggle). -->
        <svg
          data-testid={IDS.IDENTITY_EXPORT_QR}
          class="identity-qr"
          viewBox="0 0 {identityQr.side} {identityQr.side}"
          shape-rendering="crispEdges"
          role="img"
          aria-label={t.settings.identity_export.title}
        >
          <rect x="0" y="0" width={identityQr.side} height={identityQr.side} fill="#fff" />
          {#each identityQr.darkModules as m (m.key)}
            <rect x={m.x} y={m.y} width="1" height="1" fill="#000" />
          {/each}
        </svg>
      {/if}

      <button data-testid={IDS.IDENTITY_EXPORT_SHOW_QR_BUTTON} class="btn" onclick={toggleIdentityQr}>
        {identityQr ? t.settings.identity_export.hide_qr : t.settings.identity_export.show_qr}
      </button>
    </section>
  {/if}

  <!-- Recovery kit (settings.md § Recovery kit) — the RecoveryKey's Settings home,
       ratified 2026-08-01 immediately after Identity export. tui's whole family:
       section/status/create/replace/lost, the kit-in-hand phrase entry, the
       shown-once minted-kit display trio, the pending-window veto and the no-escrow
       repair (both 2026-09-26), and the sweep's own lines with their retry.
       The EPHEMERAL kit-side member-review pass (member-review-defer-button) landed
       2026-08-21 — see the block right after the h2 below;
       the PERMANENT review page, under current === 'member-review', is the sibling
       surface for a deferred or ordinary-session backlog, and was already built.
       identity-stolen-button/-confirm-field are built too, just below: web drives
       the whole succession ceremony now.
       The seven aftermath-progress lines are no longer render-only —
       run_aftermath_web files each leg's line into $lib/succession-aftermath as
       the pass runs (legs 2-7), so these read a live pass rather than a
       permanent null. A field that stays null is a leg web does not narrate yet;
       that store's per-leg doc comments say which and why. -->
  {#if $identity}
    <section class="section" data-testid={IDS.RECOVERY_KIT_SECTION}>
      <h2>{t.settings.recovery_kit.title}</h2>

      <!-- The last succession's group sweep, if one ran in this tab — the
           ceremony's own outcome, at the TOP of the section because it is the
           freshest thing here (tui's order). ID-less prose like tui's chrome:
           the ceremony's central claim is asserted as STATE
           (`data.succession_sweep`), not from the screen. The two lines are
           deliberately SEPARATE facts — eviction, and the roster the sweep
           cannot vouch for — with no combined "you are safe" verdict; which
           arm says what, and the silence on an account with no groups, are
           the shared projection's (`SweepView::copy`), never this page's.
           The degraded arms DO name the retry button here as of 2026-08-27
           (`SweepRetryAffordance::Rendered`, declared wasm-side): it is on
           screen directly below them. -->
      {#if sweepCopy?.outcome}
        <p class="muted">{resolveLocalized(sweepCopy.outcome)}</p>
      {/if}
      {#if sweepCopy?.unattested}
        <p class="muted">{resolveLocalized(sweepCopy.unattested)}</p>
      {/if}
      <!-- The retry, on the arms that owe work. Gated on `owesWork` — the
           SHARED render gate — and deliberately NOT on whether this browser
           could run the sweep (it never can): hiding it would leave the
           degraded copy above naming a control that is not on screen, the
           exact dishonesty that rule exists to prevent. Every press answers
           in words on `error-message`. -->
      {#if sweepCopy?.owesWork}
        <button data-testid={IDS.RECOVERY_KIT_SWEEP_RETRY_BUTTON} class="btn" onclick={doRetrySuccessionSweep}>
          {t.settings.recovery_kit.sweep_retry}
        </button>
      {/if}

      <!-- The EPHEMERAL kit-side member-review pass (succession-aftermath.md
           § Propagation, item (ii)) — directly under the section title, the
           freshest thing here, same ordering intent as tui's own
           recovery_elements. Renders ONLY where a sweep ran THIS session
           (ephemeralReviewActive) and only while items remain; the permanent
           page below (current === 'member-review') is where a deferred or
           ordinary-session backlog lives — same row shape, same ids, ONE
           family reused rather than twinned (this block and that page's
           {#each} are intentionally near-identical markup). -->
      {#if ephemeralReviewActive && memberReviewLoaded && memberReviewRows.length > 0}
        <p class="muted">{t.settings.recovery_kit.review_intro}</p>
        {#each memberReviewRows as row (row.review.person)}
          <div data-testid={IDS.MEMBER_REVIEW_ROW} class="filter-item">
            <span>{t.settings.recovery_kit.review_row({ who: resolveLocalized(row.text.who), reason: row.text.reasons.map(resolveLocalized).join(', ') })}</span>
            <button data-testid={IDS.MEMBER_REVIEW_KEEP_BUTTON} class="btn small" onclick={() => keepMemberReview(row.review.person)}>
              {t.settings.recovery_kit.review_keep}
            </button>
            <button data-testid={IDS.MEMBER_REVIEW_REMOVE_BUTTON} class="btn danger small" onclick={() => removeMemberReview(row.review.person)}>
              {t.settings.recovery_kit.review_remove}
            </button>
          </div>
        {/each}
        <button data-testid={IDS.MEMBER_REVIEW_DEFER_BUTTON} class="btn" onclick={deferMemberReviewPass}>
          {t.settings.recovery_kit.review_defer}
        </button>
      {/if}

      <!-- The seven aftermath-progress lines, in the order the legs run
           (settings.md § Recovery kit — leg 7 renders ABOVE leg 6). Each
           renders iff its field is non-null, mirroring linux's
           set_optional_line/is_visible gating. -->
      {#if recoveryAftermath.backupRegrant}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_BACKUP_REGRANT_STATUS}>
          {resolveLocalized(recoveryAftermath.backupRegrant)}
        </p>
      {/if}
      {#if recoveryAftermath.mlsReseal}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_MLS_RESEAL_STATUS}>
          {resolveLocalized(recoveryAftermath.mlsReseal)}
        </p>
      {/if}
      {#if recoveryAftermath.grantRemint}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_GRANT_REMINT_STATUS}>
          {resolveLocalized(recoveryAftermath.grantRemint)}
        </p>
      {/if}
      {#if recoveryAftermath.corpusReseal}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_CORPUS_RESEAL_STATUS}>
          {resolveLocalized(recoveryAftermath.corpusReseal)}
        </p>
      {/if}
      <!-- Leg 7 before leg 6, same reason the task runs them in that order:
           the burn is the only leg that takes something away, so every
           restoring leg reports above it. -->
      {#if recoveryAftermath.draftsReseal}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_DRAFTS_RESEAL_STATUS}>
          {resolveLocalized(recoveryAftermath.draftsReseal)}
        </p>
      {/if}
      {#if recoveryAftermath.mailBurn}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_MAIL_BURN_STATUS}>
          {resolveLocalized(recoveryAftermath.mailBurn)}
        </p>
      {/if}
      {#if recoveryInheritedFiltersOpen > 0}
        <p class="muted" data-testid={IDS.RECOVERY_KIT_INHERITED_FILTERS_STATUS}>
          {t.settings.recovery_kit.inherited_filters({ count: String(recoveryInheritedFiltersOpen) })}
        </p>
      {/if}

      <p class="muted" data-testid={IDS.RECOVERY_KIT_STATUS}>
        {recoveryStatus ? resolveLocalized(recoveryStatus.status_text) : t.settings.recovery_kit.status_loading}
      </p>

      <!-- Rendered while a wired ceremony can consume it — which is what makes
           it `optional_elements` in ui.yaml rather than `elements`. TWO
           ceremonies read it here: replace, and (since the web leg)
           stolen. `allows_stolen` answers true in EVERY state — succession is
           offered whatever shape the kit is in — so the field is effectively
           always up, exactly as tui renders it. The veto and the escrow repair
           read it too, and name their own arms so the gate is tui's whole one.
           ⚠ The `allows_replace`-only gate this replaces would have hidden the
           field in `NeverCreated`, the one state where replace is false and
           stolen is true: the ceremony would have been offered with no way to
           paste the kit it requires.
           ⚠ Also up with the status UNREAD or failed (`!recoveryStatus`) —
           widened to match the trigger/confirm field above, which already
           carry no status gate at all: an unread
           status must not take the field away from the ceremony that needs
           it most (the succession ceremony's authorization is the KIT, never
           the status). -->
      {#if !recoveryStatus || recoveryStatus.allows_replace || recoveryStatus.allows_stolen || recoveryStatus.allows_escrow_reseal || recoveryStatus.replacement_pending}
        <input
          data-testid={IDS.RECOVERY_ENTRY_PHRASE_FIELD}
          type="text"
          class="input"
          placeholder={t.settings.recovery_kit.kit_phrase_placeholder}
          bind:value={recoveryPhrase}
        />
      {/if}

      <div class="handle-form">
        <button
          data-testid={IDS.RECOVERY_KIT_CREATE_BUTTON}
          class="btn"
          onclick={doCreateRecoveryKit}
          disabled={recoveryBusy || !recoveryStatus?.allows_create}
        >
          {t.settings.recovery_kit.create}
        </button>
        <button
          data-testid={IDS.RECOVERY_KIT_REPLACE_BUTTON}
          class="btn"
          onclick={doReplaceRecoveryKit}
          disabled={recoveryBusy || !recoveryStatus?.allows_replace}
        >
          {t.settings.recovery_kit.replace}
        </button>
        <button
          data-testid={IDS.RECOVERY_KIT_LOST_BUTTON}
          class="btn"
          onclick={doRequestRecoveryKitLost}
          disabled={recoveryBusy || !recoveryStatus?.allows_lost}
        >
          {t.settings.recovery_kit.lost}
        </button>
      </div>

      <!-- The two affordances for a STATE, never standing buttons (tui's
           recovery_elements gates): the veto only while a seed-alone
           replacement pends, the escrow repair only in the no-escrow state —
           deliberately not replace, which would retire the kit the user holds. -->
      {#if recoveryStatus?.replacement_pending}
        <button
          data-testid={IDS.RECOVERY_PENDING_VETO_BUTTON}
          class="btn danger"
          onclick={() => doKitInHandRepair(vetoRecoveryReplacement, (message) => t.settings.recovery_kit.veto_failed({ message }))}
          disabled={recoveryBusy}
        >
          {t.settings.recovery_kit.veto}
        </button>
      {/if}
      {#if recoveryStatus?.allows_escrow_reseal}
        <button
          data-testid={IDS.RECOVERY_KIT_ESCROW_RESEAL_BUTTON}
          class="btn"
          onclick={() => doKitInHandRepair(resealRecoveryEscrow, (message) => t.settings.recovery_kit.action_failed({ message }))}
          disabled={recoveryBusy}
        >
          {t.settings.recovery_kit.escrow_reseal}
        </button>
      {/if}

      <!-- The succession ceremony (identity-succession.md § The succession
           statement). Live in EVERY kit state — `allows_stolen()` is true
           unconditionally, per settings.md § Recovery kit's "stolen (any)":
           theft does not wait for a kit to be in a convenient state, and an
           identity stolen while a replacement pends is the worst state of all.
           So there is no `recoveryStatus` gate here, unlike the three above.

           Irreversible, so it is gated behind the same type-to-confirm idiom
           account deletion uses — never a bare click. The phrase field it also
           needs is the shared `recovery-entry-phrase-field` above; it renders
           whenever the status is unread OR allows this ceremony. -->
      <p class="muted">{t.settings.recovery_kit.stolen_warning}</p>
      <input
        data-testid={IDS.IDENTITY_STOLEN_CONFIRM_FIELD}
        type="text"
        class="input"
        placeholder={t.settings.recovery_kit.stolen_confirm_placeholder}
        bind:value={stolenConfirm}
      />
      <!-- The confirm token is a fixed literal, NOT a localized string: only its
           prompt is translated (`stolen_confirm_placeholder`), so the word is
           the same on every app and in every language. tui pins it as
           `settings::recovery::STOLEN_CONFIRM_WORD` and the e2e action types it
           literally (`actions/settings.py::succeed_identity_with_held_kit`);
           this is the third copy of a spec-fixed token, and the three must
           agree. -->
      <button
        data-testid={IDS.IDENTITY_STOLEN_BUTTON}
        class="btn danger"
        onclick={doSucceedIdentity}
        disabled={recoveryBusy || stolenConfirm.trim() !== 'SUCCEED'}
      >
        {t.settings.recovery_kit.stolen}
      </button>

      {#if recoveryMinted && recoveryQr}
        <code data-testid={IDS.RECOVERY_KIT_SECRET_DISPLAY} class="mono">{recoveryMinted.secretHex}</code>
        <button data-testid={IDS.RECOVERY_KIT_SECRET_COPY_BTN} class="btn" onclick={copyRecoveryKitSecret}>
          {t.common.copy}
        </button>
        <svg
          data-testid={IDS.RECOVERY_KIT_QR}
          class="identity-qr"
          viewBox="0 0 {recoveryQr.side} {recoveryQr.side}"
          shape-rendering="crispEdges"
          role="img"
          aria-label={t.settings.recovery_kit.title}
        >
          <rect x="0" y="0" width={recoveryQr.side} height={recoveryQr.side} fill="#fff" />
          {#each recoveryQr.darkModules as m (m.key)}
            <rect x={m.x} y={m.y} width="1" height="1" fill="#000" />
          {/each}
        </svg>
      {/if}

      {#if recoveryError}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{recoveryError}</p>{/if}
    </section>
  {/if}

  <!-- Change-handle renders whenever logged in (matching linux's unconditional
       change-handle group); the form is reachable even before the handle cache
       hydrates, and the shared validator gives instant feedback. -->
  {#if $identity}
    <section class="section">
      <h2>{t.settings.account_page.change_handle}</h2>
      <div class="handle-form">
        <label for="new-handle-input">{t.settings.account_page.new_handle}</label>
        <input id="new-handle-input" data-testid={IDS.NEW_HANDLE} type="text" placeholder={t.status.change_handle.placeholder} bind:value={newHandle} class="input" />
        <button data-testid={IDS.CHANGE_HANDLE} class="btn" onclick={doChangeHandle} disabled={changingHandle || !newHandle.trim()}>
          {changingHandle ? t.status.change_handle.changing : t.common.change}
        </button>
      </div>
      {#if changeHandleError}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{changeHandleError}</p>{/if}
    </section>
  {/if}

  <section class="section">
    <h2>{t.settings.account_page.data_export}</h2>
    <p class="muted">{t.status.data_export.description}</p>
    <button class="btn" onclick={exportData} disabled={exporting} data-testid={IDS.SETTINGS_EXPORT_DATA_BUTTON}>
      {exporting ? t.events.exporting : t.settings.account_page.export_my_data}
    </button>
    {#if exportError}<p class="error">{exportError}</p>{/if}
  </section>

  {#if $identity?.handle}
    <section class="section">
      <h2>{t.common.danger_zone}</h2>
      <p class="muted small">{t.status.danger_zone.delete_hint}</p>
      <input data-testid={IDS.SETTINGS_DELETE_CONFIRM_FIELD} type="text" class="input" placeholder={t.settings.delete_confirm_placeholder} bind:value={deleteConfirmText} />
      <button data-testid={IDS.SETTINGS_DELETE_ACCOUNT_BUTTON} class="btn danger" onclick={doDeleteAccount} disabled={deleteConfirmText !== 'DELETE'}>{t.settings.account_page.delete_account}</button>
      {#if deleteAccountError}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{deleteAccountError}</p>{/if}
    </section>
  {/if}

  <!-- Pending actions (settings.md § Pending actions) — STANDING, sitting
       below the two delayed verbs this page hosts (the third, snapshot
       delete, schedules from the Backups page and appears here on the next
       Account visit's hydrate). Always present — a conditional render would
       hide the affordance exactly when a mis-clicker goes looking for it.
       The h2 itself carries the testid + the three-state honest text (bare
       title un-hydrated / empty-state line / counted title), mirroring
       tui's `pending_actions_elements` / linux's `build_pending_actions_group`
       exactly: it answers honestly, never a settled "nothing scheduled"
       claim before the first list read lands. -->
  {#if $identity}
    <section class="section">
      <h2 data-testid={IDS.PENDING_ACTIONS_SECTION}>
        {pendingActions === null
          ? t.settings.pending_actions.title
          : pendingActions.length === 0
            ? t.settings.pending_actions.none_scheduled
            : t.settings.pending_actions.title_count({ count: String(pendingActions.length) })}
      </h2>
      {#each pendingActions ?? [] as action (action.id)}
        <div data-testid={IDS.PENDING_ACTION_ITEM} class="pending-action-item">
          <span data-testid={IDS.PENDING_ACTION_DESCRIPTION}>
            {describePendingAction(action.action_type, action.target)}
          </span>
          <span data-testid={IDS.PENDING_ACTION_EXECUTE_AFTER}>
            {t.settings.pending_actions.applies({
              time: new Date(action.execute_after * 1000).toLocaleString(),
            })}
          </span>
          <button
            data-testid={IDS.PENDING_ACTION_CANCEL_BUTTON}
            class="btn danger small"
            onclick={() => doCancelPendingAction(action.id)}
          >
            {t.settings.pending_actions.cancel}
          </button>
        </div>
      {/each}
      {#if pendingActionsError}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{pendingActionsError}</p>{/if}
    </section>
  {/if}

{:else if current === 'member-review'}
  <!-- ── Members To Review — the permanent post-succession unattested-member
       review page (succession-aftermath.md § Propagation item (iv)). Renders
       whatever a review sweep left unanswered; NO sweep gate of its own — it
       is reachable, and ordinarily empty, at all times. The row shape
       (member-review-row + the Keep/Remove pair scoped inside it) is the
       same family the ephemeral kit-side pass on the 'account' sub-page
       shares — one family, two
       surfaces, reused rather than twinned. ── -->
  <section class="section">
    <h2>{t.settings.member_review_page.title}</h2>
    {#if memberReviewError}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{memberReviewError}</p>{/if}
    {#if memberReviewLoaded && memberReviewRows.length === 0}
      <!-- The page's ORDINARY state — it holds only a backlog somebody
           explicitly postponed. NOT a safety verdict
           (succession-aftermath.md § Implementation status today leaves
           deliberately no combined "is the user safe" boolean). -->
      <p data-testid={IDS.MEMBER_REVIEW_EMPTY} class="muted">{t.settings.member_review_page.empty}</p>
    {:else if memberReviewLoaded}
      <p class="muted">{t.settings.member_review_page.intro}</p>
      {#each memberReviewRows as row (row.review.person)}
        <div data-testid={IDS.MEMBER_REVIEW_ROW} class="filter-item">
          <span>{t.settings.recovery_kit.review_row({ who: resolveLocalized(row.text.who), reason: row.text.reasons.map(resolveLocalized).join(', ') })}</span>
          <button data-testid={IDS.MEMBER_REVIEW_KEEP_BUTTON} class="btn small" onclick={() => keepMemberReview(row.review.person)}>
            {t.settings.recovery_kit.review_keep}
          </button>
          <button data-testid={IDS.MEMBER_REVIEW_REMOVE_BUTTON} class="btn danger small" onclick={() => removeMemberReview(row.review.person)}>
            {t.settings.recovery_kit.review_remove}
          </button>
        </div>
      {/each}
    {/if}
  </section>

{:else if current === 'privacy'}
  <!-- ── Privacy — inbox mode / spam / moderation / email filters. ── -->
  <section class="section">
    <h2>{t.status.inbox_privacy.title}</h2>
    <p class="muted">{t.status.inbox_privacy.description}</p>
    <!-- Four unmarked radios answer nothing on their own, so the page says why
         — the same copy tui and linux surface on `error-message` in this exact
         state (`fauna_i18n::strings::settings::privacy_page::INBOX_MODE_UNKNOWN`).
         Covers both ways the mode can be unknown (the read is still out, or it
         failed): while the user is looking at four unmarked rows those are the
         same fact. -->
    {#if inboxMode === null}
      <p class="error" data-testid={IDS.ERROR_MESSAGE}>{t.settings.privacy_page.inbox_mode_unknown}</p>
    {/if}
    <div class="radio-group">
      {#each inboxModes as m}
        <label class="radio-label">
          <input data-testid="inbox-mode-{m.value}" type="radio" name="inbox-mode" value={m.value}
            checked={inboxMode === m.value}
            onchange={() => updateInboxMode(m.value)}
            disabled={inboxModeLoading} />
          <span>
            <strong>{m.label}</strong>
            <span class="muted small">{m.desc}</span>
          </span>
        </label>
      {/each}
    </div>
    {#if inboxModeError}<p class="error">{inboxModeError}</p>{/if}
  </section>

  <section class="section" data-testid={IDS.SPAM_PREFERENCES}>
    <h2>{t.status.spam.title}</h2>
    <p class="muted">{t.status.spam.description}</p>

    <div class="pref-row">
      <label class="pref-label" for="spam-threshold-input">{t.status.spam.spam_threshold}</label>
      <div class="pref-control">
        <input
          data-testid={IDS.SPAM_THRESHOLD}
          id="spam-threshold-input"
          type="range" min="0" max="1" step="0.1"
          bind:value={spamThreshold}
        />
        <span class="pref-value">{spamThreshold.toFixed(2)}</span>
        <span class="pref-hint muted">
          {t.status.spam[spamThresholdBand(spamThreshold)]}
        </span>
      </div>
    </div>

    <div class="pref-row">
      <label class="pref-label" for="phishing-threshold-input">{t.status.spam.phishing_threshold}</label>
      <div class="pref-control">
        <input
          data-testid={IDS.PHISHING_THRESHOLD}
          id="phishing-threshold-input"
          type="range" min="0" max="1" step="0.1"
          bind:value={phishingThreshold}
        />
        <span class="pref-value">{phishingThreshold.toFixed(2)}</span>
      </div>
    </div>

    <button
      data-testid={IDS.SAVE_SPAM_PREFS}
      class="btn primary"
      onclick={saveSpamPreferences}
      disabled={spamPrefsLoading}
    >
      {spamPrefsLoading ? t.common.saving : t.status.spam.save}
    </button>
    {#if spamPrefsSaved}<span class="success">{t.common.saved}</span>{/if}
    {#if spamPrefsError}<p class="error">{spamPrefsError}</p>{/if}
  </section>

  <section class="section" data-testid={IDS.MODERATION_TAB}>
    <h2>{t.settings.moderation_page.title}</h2>
    <div data-testid={IDS.MODERATION_QUEUE}>
      {#if moderationRows.length === 0}
        <p class="muted">{t.moderation.no_actions}</p>
      {:else}
        <p class="muted">{t.moderation.flagged_count({ count: String(moderationRows.length) })}</p>
        {#each moderationRows as item, i (item.content_id + '-' + i)}
          <div class="flagged-item">
            <span class="flagged-id mono">{shortId(item.content_id)}</span>
            <ContentLabelBadge label={item.category} />
            <span class="muted small">{confidencePercent(item.confidence_per_mille)}% {t.moderation.confidence}</span>
            <!-- A local detection carries no `action` (the client classifies, it never
                 enforces) → blank action column, never a fabricated label. -->
            {#if item.action !== null}
              <span class="muted small action-label">{resolveLocalized(obligationActionLabel(item.action))}</span>
            {/if}
            <button
              data-testid={IDS.TRAIN_CORRECTION_BUTTON}
              class="btn small"
              onclick={async () => {
                const id = $identity;
                if (!id) return;
                try {
                  // The tier-1 spam-model client-write switch (mail-spam.md §
                  // Encrypted-mode interaction): a server row trains sealed via the
                  // shared MailSettingsMachine when available, degrading to
                  // fauna.moderation.train; a local row's flag-removal + client-side
                  // train both happen inside this call — re-read after so the merge
                  // re-derives without it.
                  await trainModerationCorrection(id.secretHex, item.content_id, item.source);
                  localDetections = moderationLocalDetections();
                } catch (e) {
                  error = rejectionText(e, t.settings.errors.train);
                }
              }}
            >{t.moderation.correct}</button>
          </div>
        {/each}
      {/if}
    </div>
    <div data-testid={IDS.MODERATION_REPORTS_SECTION}>
      <h3>{resolveLocalized(ledgerWords.title)}</h3>
      {#if reportLedgerLoaded && reportLedger.length === 0}
        <p class="muted">{resolveLocalized(ledgerWords.empty)}</p>
      {/if}
      {#each reportLedger as row (row.report_id)}
        <div class="flagged-item">
          <span data-testid={IDS.MODERATION_REPORT_ITEM} data-status={row.status.key}>{ledgerLine(row)}</span>
          {#if row.can_withdraw}
            <button
              data-testid={IDS.MODERATION_REPORT_WITHDRAW_BUTTON}
              class="btn small"
              onclick={() => withdrawReport(row)}
            >{t.moderation.report.withdraw}</button>
          {/if}
        </div>
      {/each}
      {#if reportWithdrawStatus}<p class="muted">{reportWithdrawStatus}</p>{/if}
    </div>
  </section>

  <section class="section">
    <h2>{t.settings.privacy_page.email_filters}</h2>
    {#if emailFilters.length === 0}
      <p class="muted">{t.status.email_filters.none}</p>
    {:else}
      {#each emailFilters as filter}
        <div data-testid={IDS.FILTER_ITEM} class="filter-item">
          <span data-testid={IDS.FILTER_NAME} class="filter-name">{filter.name}</span>
          <span data-testid={IDS.FILTER_ACTION} class="badge">{resolveLocalized(emailFilterActionLabel(filter.action))}</span>
          {#if filterIsEditableFor(filter, FILTER_ACTION_KINDS)}
            <button data-testid={IDS.FILTER_EDIT} class="btn small" onclick={() => handleEditFilter(filter.id)}>{t.common.edit}</button>
          {/if}
          <!-- The post-succession review mark, and its Keep half —
               renders ONLY on a rule the aftermath carried across and the
               owner has not adjudicated. Deliberately no remove-mark button:
               FILTER_DELETE, one line down, is the Remove half
               (no-second-removal-mechanism rule). -->
          {#if filterMarks.includes(filter.id)}
            <span data-testid={IDS.FILTER_UNATTESTED_MARK} class="muted">{t.settings.privacy_page.filter_inherited}</span>
            <button data-testid={IDS.FILTER_REVIEW_KEEP_BUTTON} class="btn small" onclick={() => handleKeepFilterMark(filter.id)}>{t.settings.privacy_page.filter_keep}</button>
          {/if}
          <button data-testid={IDS.FILTER_DELETE} class="btn danger small" onclick={() => handleDeleteFilter(filter.id)}>{t.common.delete}</button>
        </div>
      {/each}
    {/if}
    {#if !showFilterForm}
      <button data-testid={IDS.ADD_FILTER_BTN} class="btn" onclick={openCreateFilterForm}>{t.settings.privacy_page.add_filter}</button>
    {:else}
      <div class="filter-form">
        <input data-testid={IDS.FILTER_NAME_INPUT} type="text" placeholder={t.settings.filter_name} bind:value={newFilterName} class="input" />
        <select data-testid={IDS.FILTER_RULE_TYPE} bind:value={newFilterRuleType} class="input">
          <option value="SenderIs">{t.status.email_filters.sender_is}</option>
          <option value="SenderDomain">{t.status.email_filters.sender_domain}</option>
          <option value="SubjectContains">{t.status.email_filters.subject_contains}</option>
          <option value="BodyContains">{t.status.email_filters.body_contains}</option>
          <option value="HeaderExists">{t.status.email_filters.header_exists}</option>
        </select>
        <input data-testid={IDS.FILTER_RULE_VALUE} type="text" placeholder={t.common.value} bind:value={newFilterRuleValue} class="input" />
        <select data-testid={IDS.FILTER_ACTION_SELECT} bind:value={newFilterAction.kind} class="input">
          <option value="Allow">{t.status.email_filters.action_allow}</option>
          <option value="Discard">{t.status.email_filters.action_discard}</option>
          <option value="Reject">{t.status.email_filters.action_reject}</option>
          <option value="Forward">{t.status.email_filters.action_forward}</option>
        </select>
        {#if newFilterAction.kind === 'Forward'}
          <input data-testid={IDS.FILTER_FORWARD_ADDRESS} type="text" placeholder={t.settings.privacy_page.forward_address} bind:value={newFilterAction.forward_address} class="input" />
          <label class="pref-label">
            <input data-testid={IDS.FILTER_KEEP_LOCAL_COPY} data-checked={newFilterAction.keep_local_copy ? 'true' : 'false'} type="checkbox" bind:checked={newFilterAction.keep_local_copy} />
            {t.settings.privacy_page.keep_local_copy}
          </label>
        {/if}
        <div class="filter-form-actions">
          {#if editingFilterId === null}
            <button data-testid={IDS.CREATE_FILTER} class="btn primary" onclick={handleCreateFilter} disabled={creatingFilter || !newFilterName.trim() || !newFilterRuleValue.trim()}>
              {creatingFilter ? t.common.creating : t.common.create}
            </button>
          {:else}
            <button data-testid={IDS.SAVE_FILTER} class="btn primary" onclick={handleSaveFilter} disabled={creatingFilter || !newFilterName.trim() || !newFilterRuleValue.trim()}>
              {t.common.save}
            </button>
          {/if}
          <button class="btn" onclick={() => { showFilterForm = false; editingFilterId = null; }}>{t.common.cancel}</button>
        </div>
      </div>
    {/if}
    {#if emailFilterError}<p class="error">{emailFilterError}</p>{/if}
  </section>

{:else if current === 'muted-words'}
  <!-- ── Muted words — the tier-1 deterministic keyword filter for conversations
       (moderation.md § Muted keywords; content-moderation-and-ranking.md § Q3).
       CRUD over the sealed, nest-opaque `fauna.state.moderation` muted-keyword
       list; the conversation collapse render that APPLIES this list lives on
       the Conversations page, not here. ── -->
  <section class="section">
    <h2>{t.muted_words.title}</h2>
    <p class="muted">{t.muted_words.description}</p>
    <div class="filter-form">
      <input
        data-testid={IDS.MUTED_WORD_INPUT}
        type="text"
        placeholder={t.muted_words.input_placeholder}
        bind:value={newMutedWord}
        class="input"
        onkeydown={(e) => { if (e.key === 'Enter') addMutedWord(); }}
      />
      <button data-testid={IDS.MUTED_WORD_ADD_BUTTON} class="btn primary" onclick={addMutedWord} disabled={!newMutedWord.trim()}>
        {t.muted_words.add}
      </button>
    </div>
    {#if mutedWordsError}<p class="error">{mutedWordsError}</p>{/if}
    <div data-testid={IDS.MUTED_WORD_LIST} class="muted-word-list">
      {#if mutedWords.loaded && mutedWords.keywords.length === 0}
        <!-- Two conditions, not one (README.md § List pages: loading is not
             empty): a page still reading paints neither rows nor this. -->
        <p data-testid={IDS.MUTED_WORD_EMPTY} class="muted">{t.muted_words.empty}</p>
      {:else}
        {#each mutedWords.keywords.map((k) => k.keyword) as word (word)}
          <div data-testid={IDS.MUTED_WORD_ITEM} class="filter-item">
            <span data-testid={IDS.MUTED_WORD_TEXT} class="filter-name">{word}</span>
            <button data-testid={IDS.MUTED_WORD_REMOVE_BUTTON} class="btn danger small" onclick={() => removeMutedWord(word)}>
              {t.muted_words.remove}
            </button>
          </div>
        {/each}
      {/if}
    </div>
  </section>

{:else if current === 'general'}
  <!-- ── General — push notifications (the web-applicable general preference). ── -->
  <!-- One opt-in toggle (settings.md § Push notifications): its state is the
       install's stored opt-in, never the browser permission, and its label
       carries the status. `data-state` is the uniform on/off reading every
       app's witness uses (`get_attr(id, "state")`). -->
  <section class="section" data-testid={IDS.PUSH_NOTIFICATIONS_SECTION}>
    <h2>{t.settings.push_notifications.title}</h2>
    <p class="muted">{t.settings.push_notifications.device_description}</p>
    <label class="confirm-toggle">
      <input
        type="checkbox"
        data-testid={IDS.PUSH_NOTIFICATIONS_OPT_IN_TOGGLE}
        data-state={pushSubscribed ? 'on' : 'off'}
        checked={pushSubscribed}
        disabled={pushLoading}
        onchange={(e) => togglePushOptIn(e.currentTarget.checked)}
      />
      <span class="confirm-toggle-label">{t.settings.push_notifications.opt_in_label}</span>
    </label>
    {#if pushError}<p class="error" data-testid={IDS.PUSH_NOTIFICATIONS_ERROR}>{pushError}</p>{/if}
  </section>

{:else if current === 'encryption'}
  <!-- ── Encryption — MLS key packages. ── -->
  <section class="section">
    <h2>{t.settings.encryption_page.title}</h2>
    {#if keyPackageCount != null}
      <div class="field">
        <span class="field-label">{t.status.encryption.key_packages}</span>
        <span>{t.status.encryption.available({ count: String(keyPackageCount) })}</span>
      </div>
      {#if keyPackageCount < 5}
        <p class="muted small">{t.status.encryption.low_keys}</p>
      {/if}
    {:else}
      <p class="muted">{t.status.encryption.checking}</p>
    {/if}
    {#if keyPackageError}
      <p class="error">{keyPackageError}</p>
    {/if}
    <button class="btn" onclick={() => { const id = $identity; if (id) publishMlsKeys(id.secretHex, id.actorId); }} disabled={publishingKeys}>
      {publishingKeys ? t.status.encryption.publishing : t.settings.encryption_page.refresh_keys}
    </button>
  </section>

{:else if current === 'devices'}
  <!-- ── Settings → Devices (sync-file-set-ui-unification, 2026-06-28): the device
       ROSTER only. Re-homed from the former top-level Devices/Peers page into the
       Settings shell (the folder wizard/list/conflicts moved to Settings → File
       sets, below). Dumb renderer of the shared DevicesMachine — devices.md. ── -->
  <DevicesSection bind:error />

{:else if current === 'folders'}
  <!-- ── Settings → Folders (sync-file-set-ui-unification, 2026-06-28): the ONE
       control plane for folders — list + create wizard + per-set config +
       conflict resolution. Merges the former top-level folder wizard/list/
       conflicts with the former `Settings → Sync` page (which it renames). Web has
       NO local-folder binding (no browser filesystem). folders.md. ── -->
  <FoldersSection bind:error />

{:else if current === 'personalization'}
  <!-- ── Settings → Personalization (content-moderation-and-ranking.md §
       Composition + § Tier-3): the unified home hubbing Feeds / Muted words /
       Community labelers. Dumb renderer of the shared LabelerCatalogMachine,
       filtered to subscribed === true — no second RPC. ── -->
  <PersonalizationSection bind:error />

{:else if current === 'labeler-catalog'}
  <!-- ── Settings → Community labelers: browse + inspect-before-subscribe +
       (un)subscribe, over the SAME shared LabelerCatalogMachine the
       Personalization home reads. ── -->
  <LabelerCatalogSection bind:error />

{:else if current === 'p2p'}
  <!-- ── P2P — web has no dedicated settings surface yet; point to Devices /
       Bridges. Follow-on: build real P2P settings content for parity. ── -->
  <section class="section">
    <h2>{t.status.p2p.title}</h2>
    <p class="muted">{t.settings.p2p_redirect}</p>
    <a href="/app/settings/devices" class="btn">{t.settings.open_devices}</a>
    <a href="/app/bridges" class="btn">{t.settings.open_bridges}</a>
  </section>

{:else if current === 'nostr'}
  <!-- Nostr — the dedicated Nostr management surface (account / content toggles /
       relays / follows / DMs), rendered by the SAME shared component the
       standalone `/nostr` route uses (priority #2). Nostr keeps its own page, the
       same treatment as mail (nostr.md § Page structure). Shares the page-level
       error-message surface via bind:error. -->
  <NostrSettingsSection bind:error />

{:else if current === 'atproto'}
  <!-- AT Protocol — the ATProto login-plane settings page (app credentials,
       connected-app sessions, the external-apps kill-switch;
       atproto-pds-full.md § App surface, F1 scope). Placed after Nostr
       (settings.md § Navigation model). Shares the page-level error-message
       surface via bind:error. -->
  <AtprotoSettingsSection bind:error />

{:else if current === 'subscription-settings'}
  <!-- Subscriptions (consumer side — this user's subscriptions across creators,
       Slice B). Shares the page-level error-message surface via bind:error. -->
  <SubscriptionsSection bind:error />

{:else if current === 'web'}
  <!-- Web (per-user web-content hosting — the subdomain opt-in toggle).
       Shares the page-level error-message surface via bind:error. -->
  <WebSettingsSection bind:error />

{:else if current === 'mail-settings'}
  <!-- Mail (user-facing mail-credential lifecycle). Shares the page-level
       error-message surface via bind:error. -->
  <MailSettingsSection bind:error />

{:else if current === 'mail-aliases'}
  <!-- Mail aliases (exact / wildcard / disposable). -->
  <MailAliasesSection bind:error />

{:else if current === 'mail-spam'}
  <!-- Mail spam (per-account classifier — reset model, baseline opt-in, undo). -->
  <MailSpamSection bind:error />

{:else if current === 'mail-export'}
  <!-- Mail export (mailbox-export wizard). -->
  <MailExportSection bind:error />

{:else if current === 'mail-import'}
  <!-- Mail import (mailbox-import wizard — the export twin's mirror image). -->
  <MailImportSection bind:error />

{:else if current === 'mail-lists' || current === 'mail-list-members'}
  <!-- Mail lists + the per-list members drill-down (an in-section reveal). ONE
       instance serves both rail sub-pages so the drill-down selection survives
       moving between mail-lists ↔ mail-list-members. `subpage` lets the ONE
       instance render an honest empty members view when `mail-list-members` is
       reached directly, with no list ever selected. -->
  <MailListsSection bind:error subpage={current} />

{:else if current === 'nests'}
  <!-- Nests (per-user nest pairing + the nest-trust facet). The route slug is
       `nests` — the cross-app nav id the shared e2e action sends every app. -->
  <NestsSection bind:error />

{:else if current === 'task-delegation'}
  <!-- Task delegation — per heavy task kind, its current runner + the user's
       assignment (participants.md § Task delegation). -->
  <TaskDelegationSection bind:error />

{:else if current === 'connected-apps'}
  <!-- Connected apps — the roster of everything acting for the user from
       outside the apps, the Requests tray, Connect an app, Blocked apps
       (connected-apps.md). -->
  <ConnectedAppsSection bind:error />

{:else if current === 'logs'}
  <!-- ── Logs — the client's durable in-app log record (observability.md
       § Surfaces): the process-global `fauna_log` ring, newest-first, with a
       severity filter, copy, and clear. Ring-only on web (no on-disk file —
       § Persistence & privacy — Web). The shared `LogsView` renders here and on
       the admin Logs page. `error-message` is the page-level MessageBanner above
       (reads are synchronous + rarely fail). ── -->
  <section class="section" data-testid={IDS.SETTINGS_LOGS}>
    <h2>{t.logs.title}</h2>
    <p class="muted small">{t.logs.description}</p>
    <LogsView entries={logEntries} showClear onClear={clearLogs} />
  </section>
{/if}
</div>

<style>
  /* Multi-account switcher rows: the activate button fills the row, so a tap on
     "the row" (the account-switcher-item view) lands on it; the Stage-2 toggle +
     remove sit compact at the right edge. */
  .account-row { display: flex; align-items: center; gap: 0.5rem; margin: 0.25rem 0; }
  .account-row .account-main {
    flex: 1; text-align: left; padding: 0.5rem 0.75rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg-surface); color: var(--text); cursor: pointer;
  }
  .account-row .account-main:disabled { cursor: default; }
  .account-row.active .account-main { border-color: var(--accent, #3b82f6); }
  .active-badge { margin-left: 0.5rem; font-size: 0.75rem; color: var(--text-muted); }
  .confirm-toggle { display: inline-flex; align-items: center; gap: 0.35rem; white-space: nowrap; }
  .confirm-toggle-label { font-size: 0.75rem; color: var(--text-muted); }
  /* Stage-2 re-auth confirm modal (mirrors the media page's
     file-version-restore-confirm-modal family styling). */
  .modal-backdrop {
    position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4);
    display: flex; align-items: center; justify-content: center; z-index: 100;
  }
  .modal {
    background: var(--bg, #fff); padding: 1.5rem; border-radius: 8px;
    max-width: 560px; max-height: 80vh; overflow-y: auto;
  }
  .modal-actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 1rem; }
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.75rem; font-size: 1.125rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; margin-top: 0.25rem; }
  .error { color: var(--danger); font-size: 0.875rem; margin-top: 0.25rem; }
  .success { color: var(--success, #22c55e); font-size: 0.875rem; margin-left: 0.5rem; }
  .field { margin-bottom: 1rem; }
  .field .field-label { display: block; color: var(--text-muted); font-size: 0.875rem; margin-bottom: 0.25rem; }
  .field-group { margin-bottom: 1.25rem; padding-bottom: 1rem; border-bottom: 1px solid var(--border); }
  .field-group:last-child { border-bottom: none; }
  .field-group .field { margin-bottom: 0.5rem; }
  .mono { font-family: monospace; font-size: 0.8rem; word-break: break-all; }
  .input {
    width: 100%; max-width: 480px; padding: 0.5rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg); color: var(--text);
    font-family: monospace; font-size: 0.875rem;
  }
  .storage-bar { height: 8px; background: var(--bg-surface); border: 1px solid var(--border); border-radius: 4px; overflow: hidden; margin-bottom: 0.25rem; }
  .storage-fill { height: 100%; background: var(--accent); border-radius: 4px; transition: width 0.3s; }
  .storage-label { font-size: 0.8rem; margin: 0; }
  .radio-group { display: flex; flex-direction: column; gap: 0.75rem; }
  .radio-label { display: flex; align-items: flex-start; gap: 0.5rem; cursor: pointer; }
  .radio-label input { margin-top: 0.25rem; }
  .radio-label span { display: flex; flex-direction: column; gap: 0.125rem; }
  .pref-row { display: flex; align-items: center; gap: 0.75rem; margin-bottom: 0.75rem; flex-wrap: wrap; }
  .pref-label { font-size: 0.875rem; min-width: 10rem; }
  .pref-control { display: flex; align-items: center; gap: 0.5rem; }
  .pref-value { font-family: monospace; font-size: 0.875rem; min-width: 2rem; }
  .pref-hint { font-size: 0.75rem; }
  input[type="range"] { width: 10rem; }
  .handle-form { display: flex; gap: 0.5rem; align-items: center; margin-bottom: 0.5rem; flex-wrap: wrap; }
  /* The QR stays dark-on-light in BOTH themes — the white background is painted in the
     SVG, not inherited: a theme-inverted QR does not scan. */
  .identity-qr { display: block; width: 220px; height: 220px; margin: 0.5rem 0; border-radius: 4px; }
  .flagged-item {
    display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem;
    border: 1px solid var(--border); border-radius: 6px; margin-bottom: 0.375rem;
  }
  .flagged-id { flex: 1; font-size: 0.8rem; }
  .filter-item {
    display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem;
    border: 1px solid var(--border); border-radius: 6px; margin-bottom: 0.375rem;
  }
  .filter-name { font-weight: 600; flex: 1; }
  .badge {
    font-size: 0.75rem; padding: 0.125rem 0.5rem; border-radius: 4px;
    background: var(--bg-hover); color: var(--text-muted);
  }
  .filter-form {
    display: flex; flex-direction: column; gap: 0.5rem; max-width: 400px;
    margin-top: 0.5rem; padding: 0.75rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg-surface);
  }
  .filter-form-actions { display: flex; gap: 0.5rem; }
  .btn {
    padding: 0.5rem 1rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    font-size: 0.875rem; margin-top: 0.5rem; margin-right: 0.5rem;
    display: inline-block; text-decoration: none;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.primary:hover { background: var(--accent-hover); }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  .btn.small { padding: 0.25rem 0.625rem; font-size: 0.8rem; margin-top: 0; }
  .btn-copy {
    padding: 0.125rem 0.5rem; font-size: 0.75rem; margin-left: 0.5rem;
    border: 1px solid var(--border); border-radius: 4px;
    background: var(--bg-surface); color: var(--text-muted); cursor: pointer;
  }
  .btn-copy:hover { background: var(--bg-hover); }
  .build-info {
    margin-top: 2rem;
    padding-top: 1.5rem;
    border-top: 1px solid var(--border);
  }
  .build-info a { color: var(--accent); text-decoration: none; }
  .build-info a:hover { text-decoration: underline; }
</style>
