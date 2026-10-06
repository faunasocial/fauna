<script lang="ts">
  // Unified Conversations page (web) — one surface for every DM rail, rendered
  // entirely off the shared (wasm) `ConversationsManager` snapshot. The browser
  // twin of linux `src/views/conversations/` (list + detail + compose + picker +
  // overlays). Observer-driven off the snapshot — no client-side state machine,
  // never branches on `rail`, only on `capabilities.*`
  // (`docs/goal/ui/conversations.md` § Architectural rules).
  //
  // The manager singleton + reactive stores + the SMTP receive poll live in
  // `$lib/conversations`; this file is pure rendering + event→mutator glue.

  import { onMount, onDestroy } from 'svelte';
  import { identity, onActorChange } from '$lib/store';
  import { registerActorScopedReset } from '$lib/actorScope';
  import {
    ensureWasm,
    legalTakedownTombstone,
    matchesMutedKeywords,
    backupAuditObserve,
  } from '$lib/wasm';
  import { mutedKeywordsList, type MutedKeyword } from '$lib/rpc';
  import { contentRender, hydrateContentPolicy } from '$lib/contentPolicy.svelte';
  import { registerRegionBlockCounter } from '$lib/region.svelte';
  import RegionPlaceholder from '$lib/components/RegionPlaceholder.svelte';
  import type { ContentRender, RegionPlaceholder as RegionPlaceholderValue } from '$lib/wasm';
  import { notifyBuffer } from '$lib/familyNotify';
  import {
    getConversationsManager,
    refreshConversations,
    conversationsSnapshot,
    scheduleDraftSave,
    markMessageSpam,
    memberReviewKeep,
    engineRoleDenied,
    receiveRailStalled,
    unopenableMailCount,
  } from '$lib/conversations';
  import { conversationsDisplayError } from '$lib/conversations-display-error';
  import { memberReviewRoster, refreshMemberReviewRoster } from '$lib/member-reviews';
  import { consumePendingSearchNav } from '$lib/search';
  import { t } from '$lib/i18n/strings';
  import { documentToHtml, documentHasBlockedRemoteImages, attachmentBlocks, quotedMessageBlock, resolvingLinkPreviewUrls, resolvedLinkPreviews } from '$lib/document';
  import { nodeUrl } from '$lib/api';
  import LinkPreviewCard from '$lib/components/LinkPreviewCard.svelte';
  import ContentLabelBadge from '$lib/components/ContentLabelBadge.svelte';
  import { typedAddressDisplay, threadLabelDisplay, recipientResolveStatus, nextSortOrder, quicksetEmojis, contentLabelBadgeFor } from '$lib/wasm';
  // The room model's render mappings — the wasm twins of the same
  // `fauna_conversations::room` helpers tui and linux call directly and the
  // UniFFI apps reach through their own twins, so this page hand-types no
  // token, label or choice list (`ui/conversations.md` § Element IDs).
  import {
    roomClassLabel,
    roomClassAttrToken,
    guardianStateLabel,
    guardianStateAttrToken,
    roomRoleAttrToken,
    roomMemberChipText,
    roomJoinRuleEditorChoices,
    roomHistoryPolicyEditorChoices,
    roomJoinRuleToken,
    roomHistoryPolicyToken,
    roomProspectiveClass,
  } from '$lib/wasm';
  import { resolveLocalized, type LocalizedText } from '$lib/i18n/localized';
  import { conversationTimestamp, byteSize } from '$lib/value-format';
  import { sourceGlyphEmoji } from '$lib/source-glyph';
  import { toArrayBufferView } from '$lib/bytes';
  import MarkdownToolbar from '$lib/components/MarkdownToolbar.svelte';
  import MarkdownEditor from '$lib/components/MarkdownEditor.svelte';
  import type { WasmConversationsManager } from '../../../static/fauna_wasm.js';
  import { IDS } from '$lib/generated/uiIds';

  let manager: WasmConversationsManager | null = $state(null);
  let ready = $state(false);
  // Unregisters the actor-scope reset + change handler this page installs in
  // `onMount` (see there). Held so `onDestroy` can drop both — a component-local
  // closure that outlived its component would keep clearing state that no longer
  // has a page behind it.
  let cleanupActorScope: (() => void) | null = null;
  let pageError = $state('');
  // The compose editor's toolbar splice hook, bound out of `MarkdownEditor` once it
  // mounts (replaces the old `textarea` ref the toolbar drove directly).
  let editorWrap = $state<((prefix: string, suffix: string) => void) | null>(null);
  // Per-editor marker-visibility toggle:
  // compose hides inline markers by default; this flips to the all-dimmed live-preview. Client-local,
  // no persistence (the user chose a per-editor inline toggle, not a global setting).
  let composeMarkersShown = $state(false);

  // Rename overlay is client-local UI state (the manager has no "renaming" flag).
  let renaming = $state(false);
  let renameValue = $state('');
  // The room policy editor's staged draft — an OPAQUE value owned by shared
  // Rust (`fauna_conversations::RoomSettingsDraft`). The SPA hands it back and
  // forth across the wasm staging calls and never reads a field of it, so the
  // editor's rules stay decided in exactly one place for all seven apps.
  let roomSettingsOpen = $state(false);
  let roomDraft: unknown = $state(null);
  // Reply "To" line add-input buffer (client-local; the committed recipients
  // live in the shared `compose.reply_recipients`).
  let replyRecipientInput = $state('');

  // ── reactive snapshot ───────────────────────────────────────────────
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let snap = $derived($conversationsSnapshot as any);
  let threads = $derived(snap?.threads ?? []);
  let selectedId = $derived(snap?.selected_thread_id ?? null);
  let newCompose = $derived(snap?.new_thread_compose ?? null);
  let addPart = $derived(snap?.add_participant ?? null);

  // New-thread compose takes the detail pane over a selected thread (mirrors
  // linux `detail.rs::render`'s stack-page precedence).
  let mode = $derived(newCompose ? 'new' : selectedId ? 'thread' : 'empty');

  // Detail of the selected thread — re-read on every snapshot refresh
  // (`refreshConversations` sets a fresh snapshot object each tick, so reading
  // `snap` here makes this re-derive). `threadDetail` is a plain wasm getter.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let detail = $derived.by<any>(() => {
    void snap;
    if (!manager || !selectedId) return null; // manager-gate-ok: $derived.by read, not a user action
    try {
      return manager.threadDetail(selectedId);
    } catch {
      return null;
    }
  });

  // ── Post-succession member review (succession-aftermath.md § Propagation →
  // *MLS groups*, item 3a) ────────────────────────────────────────────────────
  //
  // The review mark each member chip carries, **index-parallel with
  // `participant_displays`** — so `memberMarks[i]` answers for
  // `thread-member-chip[i]` and nothing here has to predict which chip a
  // flagged person landed on. Each slot is that person's actor id (hex) or
  // `null`: the id is what *Keep* is keyed on, so answering both halves at once
  // is what keeps identity mapping out of this file — an actor id crosses the
  // wasm boundary as a 32-number array while the roster crosses as hex.
  //
  // The decision is Rust's (`memberReviewMarksForThread` →
  // `fauna_conversations::member_review_flags` → `fauna_core::data::is_under_review`,
  // the same projection tui asks per row). Re-derives on a roster change or a
  // snapshot tick (via `detail`), and the roster itself is a cache refreshed at
  // exactly the two ratified points — see `$lib/member-reviews`.
  let memberMarks = $derived.by<(string | null)[]>(() => {
    const roster = $memberReviewRoster;
    if (!manager || !selectedId || roster.length === 0) return []; // manager-gate-ok: $derived.by read, not a user action
    void detail;
    try {
      return (
        (manager.memberReviewMarksForThread(selectedId, roster) as (string | null)[] | null) ?? []
      );
    } catch {
      return [];
    }
  });

  // Each chip's participant actor id (hex), **index-parallel with
  // `participant_displays`** exactly as `memberMarks` is — so
  // `participantActorIds[i]` answers for `thread-member-chip[i]` and nothing
  // here predicts which chip a person landed on. `null` on a rail that carries
  // no actor id (mail membership is immutable, so there is nothing to remove).
  //
  // The mapping is Rust's (`participantActorIdsForThread` →
  // `TypedAddress::person_actor_id`, the same projection the thread-list column
  // and tui's member-chip join ask): an `ActorId` reaches JS as a 32-number
  // array, and hex-encoding it here is the identity mapping that boundary
  // exists to keep out of this file. Re-derives on a roster change or a
  // snapshot tick (via `detail`).
  let participantActorIds = $derived.by<(string | null)[]>(() => {
    if (!manager || !selectedId) return []; // manager-gate-ok: $derived.by read, not a user action
    void detail;
    try {
      return (
        (manager.participantActorIdsForThread(selectedId) as (string | null)[] | null) ?? []
      );
    } catch {
      return [];
    }
  });

  // Record **Keep** for the person chip `index` names, then re-read the roster —
  // the second of the two ratified refresh points. Remove has no twin here: the
  // chip itself already is it, exactly as `nest-trust-grant-revoke` is the grant
  // plane's Remove half (§ Propagation → *Removing a flagged member*).
  //
  // A `false` result is a success no-op, not a rejection: a peer device may have
  // already answered, and an adjudicated item is kept at rest carrying its
  // verdict either way — so the re-read is what clears the mark, in both cases.
  async function keepMember(person: string) {
    const id = $identity;
    if (!id) return;
    try {
      await memberReviewKeep(person);
      await refreshMemberReviewRoster();
    } catch (e) {
      pageError = e instanceof Error ? e.message : String(e);
    }
  }

  // The member chip **is** Remove on a membership-change-capable thread
  // (`conversations.md` § the `thread-member-chip[i]` row) — the affordance tui,
  // linux and apple have carried all along and web rendered an inert `<span>`
  // for until 2026-09-01, because no test had ever clicked it here (the
  // mock-backed membership suite covers only add).
  //
  // The identity comes from the zip captured at PAINT, never from the index at
  // click time — the same reason tui's chip carries the row's own address
  // instead of its position. `removeParticipant` keys both of its halves on the
  // actor id, so a roster that shifted between paint and tap still removes the
  // person this chip named rather than whoever now stands in that slot; picking
  // by index is the wrong-person removal this shape makes unrepresentable.
  //
  // The handle half is `participant_displays[index]` — a Fauna address's
  // `display()` IS its handle — and is cosmetic to the removal itself (an MLS
  // roster's handles are empty by construction and attacker-chosen by threat
  // model). It matters only because a REFUSED removal puts the row back, and it
  // should go back reading the way it did.
  function removeMember(index: number) {
    const id = selectedId;
    const actorIdHex = participantActorIds[index];
    // No actor id means a rail with no removable identity beneath the address;
    // the chip is not a control there and the capability gate already hid it.
    if (!id || !actorIdHex) return;
    const handle = detail?.participant_displays?.[index] ?? '';
    run(async () => {
      await manager?.removeParticipant(id, handle, actorIdHex);
    });
  }

  // og:image preview blobs are served by the user's OWN nest (the nest fetched +
  // stored the content-addressed image at resolve time — render-model.md § D4); the
  // card loads it from `/api/v1/blob/<hash>` exactly as the feed does, gated on the
  // shared `revealed` flag the D3 reveal walk projects.
  function previewBlobUrl(hash: string): string {
    return `${nodeUrl()}/api/v1/blob/${hash}`;
  }

  // Resolved link-preview cards for a bubble's document — the conversations twin of
  // feed `PostCard`'s `previewCards`, now the same one-line read of the shared
  // projection. `Resolving`/`Failed` paint no card (the producer's inline body link
  // already shows the URL — render-model.md § D4); that state match is shared Rust's.
  const previewCards = resolvedLinkPreviews;

  // Fire `fauna.linkpreview.resolve` once per bare-url `LinkPreview { Resolving }`
  // block in the open thread — the conversations twin of the feed's augmentPost
  // trigger (`manager.resolveLinkPreview`). The manager caches + folds the terminal
  // state; `refreshConversations` pulls it into the next `threadDetail`. Dedup by url
  // so a notify-driven re-render never re-fires (Resolved blocks aren't `Resolving`).
  const previewResolving = new Set<string>();
  $effect(() => {
    const d = detail;
    if (!manager || !d) return; // manager-gate-ok: $effect ordering guard, not a user action
    for (const msg of d.messages) {
      for (const url of resolvingLinkPreviewUrls(msg.document)) {
        if (!previewResolving.has(url)) {
          previewResolving.add(url);
          void manager.resolveLinkPreview(url).then(() => refreshConversations());
        }
      }
    }
  });

  // ── the backup audit's observation feed (backups.md § Audit-alert surface) ──
  //
  // The conversation list is where this client shows the user what it knows about
  // the nest-originated message kinds, so it is the honest place to say "I have
  // seen a record this new". Freshness compares the destination against **this**
  // observation, never against a live read of the source nest — a hostile or
  // lagging source answering "nothing new" would otherwise make freshness
  // unfailable forever, and what this device saw with its own eyes it cannot be
  // argued out of afterwards.
  //
  // ⚠ This is the load-bearing half of the audit. A client that renders the
  // Backups page's audit elements but never feeds this ships a permanently-passing
  // audit, and nothing about it looks broken.
  //
  // The in-memory high-water skips the `localStorage` round trip on the common
  // no-op (the shared `observe_local_record` is monotonic, so a repeat render of
  // the same threads would write nothing anyway). **Keyed by actor** because web
  // switches accounts in-process: a process-global cache would let one account's
  // observation silently suppress another's.
  let observed: { actor: string; highWaterMs: number } | null = null;
  $effect(() => {
    const id = $identity;
    if (!id || threads.length === 0) return;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const newest = threads.reduce((m: number, t: any) => Math.max(m, t.last_activity_ms ?? 0), 0);
    if (newest <= 0) return;
    if (observed?.actor === id.actorId && observed.highWaterMs >= newest) return;
    observed = { actor: id.actorId, highWaterMs: newest };
    // Never surfaced: a missed observation costs at most a weaker freshness
    // comparison until the next render, and there is nothing a user could do.
    try {
      backupAuditObserve(id.actorId, newest);
    } catch {
      /* best-effort, exactly like the native shells' logged-and-dropped failure */
    }
  });

  // Compose state + capabilities for the active compose bar. In new-thread mode
  // there's no rail yet, so caps is null → every affordance enabled (linux
  // renders the new-thread compose bar with `caps = None`).
  let activeCompose = $derived(mode === 'new' ? newCompose : detail?.compose ?? null);
  // What `dm-reply-preview` says: the shared `ConversationsManager::reply_preview`
  // (the answered message's sender + plain-text excerpt), rendered as tui and
  // linux render it and never derived here. It painted the bare message id until
  // 2026-09-24; an answered message outside the fetched window previews empty
  // rather than stale or wrong. Re-read with `detail`, which re-derives per snapshot.
  let replyPreviewText = $derived.by<string>(() => {
    void detail;
    if (!manager || !selectedId || mode !== 'thread') return ''; // manager-gate-ok: $derived.by read, not a user action
    try {
      const p = manager.replyPreview(selectedId) as { sender_display: string; excerpt: string } | null;
      return p ? `${p.sender_display}: ${p.excerpt}` : '';
    } catch {
      return '';
    }
  });
  let caps = $derived(mode === 'thread' ? detail?.capabilities ?? null : null);
  // Inline markdown styling + the toolbar are active only on rails that support
  // markdown (new-thread mode has no rail yet → enabled, matching the toolbar's
  // historical `caps ? supports_markdown === false : false` disabled rule).
  let markdownEnabled = $derived(caps ? caps.supports_markdown !== false : true);
  // Add-participant overlay picker takes priority over the new-thread picker.
  let activePicker = $derived(addPart?.picker ?? newCompose?.recipient_picker ?? null);

  // Page-level error surface (conversations.md § Errors & edge cases). Two
  // manager-owned truths feed the one `error-message` element, and they do not
  // overlap: `snap.error` carries the **membership/label** wire ops
  // (confirm_add_participant / remove_participant / rename_thread);
  // `send_state` carries **sends**. Read the page error FIRST — not because it
  // matters more, but because it is always the more recent of the two BY
  // CONSTRUCTION (every producer clears it on entry, send/send_new_thread
  // included, so a stale membership failure can never mask a fresh send
  // failure). Mirrors tui's `sync_page_error` / linux's `page_error_text` —
  // read either's doc comment before touching this precedence.
  let membershipError = $derived.by<string | null>(() => {
    const err = snap?.error as LocalizedText | null | undefined;
    return err ? resolveLocalized(err) : null;
  });
  // A failed compose-send is the shared `ComposeState.send_state == Failed {
  // reason }` on the active compose (serializes to `{ Failed: { reason } }`;
  // Idle/Sending are plain strings). Mirrors linux `detail.rs::render`'s
  // active-compose read — no client-side state machine. `reason` is a
  // `LocalizedText` (key + `{message}` args), same as the page error, so it
  // resolves through the same pipeline — never painted raw.
  let sendFailure = $derived.by<string | null>(() => {
    const ss = (activeCompose as { send_state?: unknown } | null)?.send_state;
    return ss && typeof ss === 'object' && 'Failed' in ss
      ? resolveLocalized((ss as { Failed: { reason: LocalizedText } }).Failed.reason)
      : null;
  });
  // `pageError` is a THIRD, web-specific channel — a JS exception `run()`
  // itself caught (not a manager-tracked failure) — kept as the last-resort
  // fallback; it never overlaps with the two manager-owned truths above.
  //
  // The engine-role refusal OUTRANKS all three, exactly as linux's
  // `page_error_text` and apple's `ConversationsVM.pageErrorText` rank their
  // `engine_served_elsewhere` / `engineServedElsewhere` read first, and for the
  // same reason those two state: with no engine there is no wire op to fail, so
  // the honest standing "served elsewhere" must never be masked by an unrelated
  // gesture's outcome. Web reaches the same truth by a different route — a
  // refused Web Locks acquire rather than a manager field — because web's losing
  // tab builds no manager to ask (`$lib/conversations`'s `engineRoleDenied`,
  // `account-scoping.md` § Concurrent instances → *Web*). Same string as every
  // other app: `t.conversations.errors.served_elsewhere`, which reads correctly
  // for a tab as it does for a native instance.
  let roleRefusal = $derived($engineRoleDenied ? t.conversations.errors.served_elsewhere : null);
  // A stalled receive pump (`$lib/receive-pump`) — a pass that never finished,
  // web's shape of a receive loop that died — is the second standing truth, and
  // ranks directly under the role refusal for the same reason: an unrelated
  // gesture's outcome must never mask it (`ui/conversations.md` § Errors & edge
  // cases). Same string as every app's dead-rail notice.
  let receiveStopped = $derived(
    $receiveRailStalled ? t.conversations.errors.receive_stopped : null,
  );
  // The floor of the stack (`ui/conversations.md` § Errors & edge cases → *A
  // fifth truth*): received mail this run that could not open under the
  // account's key set, ranked below every other truth here so a fresh failure
  // of any of them outranks it, and cleared only as records retire their own
  // entry (never by an unrelated success fold).
  let unopenableMail = $derived(
    $unopenableMailCount > 0
      ? t.conversations.errors.mail_unopenable({ count: String($unopenableMailCount) })
      : null,
  );
  let displayError = $derived(
    conversationsDisplayError({
      roleRefusal,
      receiveStopped,
      membershipError,
      sendFailure,
      pageError,
      unopenableMail,
    }),
  );

  /** Drop this page's actor-scoped caches (`$lib/actorScope`).
   *
   *  Registered rather than tied to the component lifecycle because an in-app
   *  actor switch is a same-route `goto`, which does NOT remount this component —
   *  see the `onActorChange` note below. Everything here is one identity's
   *  content: `previewResolving` doubles as the resolve-once guard (so a stale
   *  entry suppresses the incoming actor's resolve forever, the feed page's bug), and `attachmentUrls` holds blob URLs over DECRYPTED attachment
   *  bytes, which the isolation contract forbids the next account reaching at all.
   *
   *  The blob URLs are revoked, not merely dropped: releasing the reference is
   *  what actually frees the decrypted bytes, and an un-revoked URL stays fetchable
   *  from the document for as long as it lives. */
  function resetActorScopedCaches(): void {
    previewResolving.clear();
    for (const url of attachmentUrls.values()) URL.revokeObjectURL(url);
    attachmentUrls.clear();
    mutedKeywords = [];
    revealedMuted = new Set();
    revealedContent = new Set();
  }

  onMount(async () => {
    // REGISTERED BEFORE THE FIRST AWAIT, deliberately. `onActorChange` also
    // fires the handler immediately when an identity is already present, and
    // that fallback is what made a late registration look correct — but it
    // only ever covers "the identity was already there", never "the identity
    // arrived while I was booting". Awaiting first opens a window the width of
    // a whole wasm module instantiation, in which an identity change fires no
    // drop and no rebuild for this page and NOTHING says so — the silent-by-
    // construction outcome `account-scoping.md` § The scoping taxonomy bans.
    // `media/+page.svelte` and `+layout.svelte` already register this early;
    // this page is held to the same shape by
    // `$lib/actor-scope-registration-contract.test.ts`. Nothing below needs
    // wasm to REGISTER — the handlers await it themselves where they need it.
    identity.init();

    const offReset = registerActorScopedReset(resetActorScopedCaches);

    // Rebuild for whoever is signed in, now and on every later actor CHANGE.
    //
    // This used to be a one-shot build in `onMount`, which is correct only while
    // the component is guaranteed to unmount between actors. It is not: an in-app
    // switch navigates to the route it is already on, and SvelteKit resolves that
    // same-route `goto` without remounting — so `onMount` never ran again and the
    // page kept driving the OUTGOING actor's manager. The thread LIST still looked
    // right (it renders from the module-level snapshot store, which follows the
    // correctly-rebuilt singleton), so the only symptom was threads that open
    // empty with no error. The page's own comment below asserting it "remounts on
    // re-navigation" is true and was exactly the blind spot: re-navigation is not
    // the case that breaks, a same-route switch is.
    //
    // `onActorChange` runs after every registered reset, so the caches above are
    // already clear here (account-scoping.md § The scoping taxonomy, in-memory
    // corollary).
    const offActor = onActorChange(async () => {
      try {
        manager = await getConversationsManager();
        refreshConversations();
        // A `SearchNav::Draft`/`SearchNav::Mail` deep-link left by the Search
        // page (`$lib/search`'s `consumePendingSearchNav`,
        // `../search/+page.svelte`'s `openResult`) — routed through the SAME
        // local gestures a direct `conversation-item` click / "new
        // conversation" button use, so this is not a second mutation path
        // (`ui/search.md` § Where logic lives → *Result navigation*).
        // `Mail` lands the thread jump only — no wasm export for the
        // message-select half of its contract yet (see search page's
        // `navigableTarget` doc comment).
        const pendingNav = consumePendingSearchNav();
        if (pendingNav?.kind === 'thread') selectThread(pendingNav.threadId);
        else if (pendingNav?.kind === 'compose') newConversation();
      } catch {
        // Not logged in / not connected yet — the page shows the identity hint;
        // the layout's poll builds the manager once an identity appears.
      }
      // Re-read the incoming actor's own muted words + content policy: both are
      // account-scoped, and both are read here rather than inherited.
      void loadMutedKeywords();
      void loadContentPolicy();
    });

    cleanupActorScope = () => { offReset(); offActor(); };

    // Only now the module itself: the seam above is live, so an identity that
    // lands during this instantiation is already heard.
    await ensureWasm();
    quickSetEmoji = quicksetEmojis();
    ready = true;
  });

  // ── Muted words apply (moderation.md § Muted keywords; content-moderation-
  // and-ranking.md § Q3) — Option A (per-app, at-render, session-local
  // reveal; NOT a shared-Rust change, NOT a spam-queue flag). The muted list
  // is loaded once when this view mounts (SvelteKit remounts this component on
  // re-navigation to /app/conversations, so returning from the Settings
  // muted-words sub-page picks up the latest saved list). `isMuted` matches at
  // RENDER time via the shared `matchesMutedKeywords`, over the CURRENT list —
  // edits are reflected immediately for any not-yet-revealed message.
  let mutedKeywords = $state<MutedKeyword[]>([]);
  let revealedMuted = $state<Set<string>>(new Set());

  async function loadMutedKeywords() {
    const id = $identity;
    if (!id) return;
    try {
      // The collapse wants only the terms; the record's `loaded` bit belongs to
      // the Settings page's empty state, and there is no empty state here.
      mutedKeywords = (await mutedKeywordsList(id.secretHex)).keywords;
    } catch {
      // No identity / not reachable yet — leave the list empty (no collapse).
    }
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isMuted(msg: any): boolean {
    if (mutedKeywords.length === 0) return false;
    return matchesMutedKeywords(msg.body ?? '', mutedKeywords);
  }

  function revealMuted(messageId: string): void {
    revealedMuted = new Set(revealedMuted).add(messageId);
  }

  // ── Content policy (family-safety.md § Content policy) — the shared render
  // verdict over the message's own `labels`, composing the viewer's spam/phishing
  // thresholds with any guardian floor (strictest-wins, in Rust). The hydration
  // mirrors `loadMutedKeywords`; a late-landing read re-renders via the module
  // `$state`. A `block` floor renders ahead of the muted arm (absolute); a
  // `collapse` (own threshold or guardian `collapse`) renders after it, revealable.
  async function loadContentPolicy() {
    const id = $identity;
    if (!id) return;
    await hydrateContentPolicy(id.secretHex);
    notifyBuffer.setIdentity(id.secretHex);
  }
  let revealedContent = $state<Set<string>>(new Set());
  function revealContent(messageId: string): void {
    revealedContent = new Set(revealedContent).add(messageId);
  }
  // The shared render for a message — the region, the guardian floor and the
  // viewer's own thresholds composed in Rust (`contentRender`), over the
  // post-decrypt body (the zero author, as on tui/linux). A REGION placeholder
  // paints ahead of every other arm; its collapse shares the family reveal set.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function msgRender(msg: any): ContentRender {
    return contentRender(msg.labels, {
      contentIdHex: msg.message_id,
      authorHex: null,
      text: msg.body ?? '',
      hashtags: [],
      hasMedia: false,
    });
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function regionPlaceholderMsg(msg: any): RegionPlaceholderValue | null {
    const placeholder = msgRender(msg).placeholder;
    if (!placeholder) return null;
    return placeholder.verb === 'block' || !revealedContent.has(msg.message_id) ? placeholder : null;
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isContentBlockedMsg(msg: any): boolean {
    return msgRender(msg).verdict === 'block';
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function isContentCollapsedMsg(msg: any): boolean {
    return !revealedContent.has(msg.message_id) && msgRender(msg).verdict === 'collapse';
  }
  // Convention 17's verdict-side walk (`region-block-never-silent`): the open
  // thread's messages the region blocks — a deleted or legally withdrawn one
  // renders its own tombstone and carries no body to withhold (tui's walk).
  onMount(() =>
    registerRegionBlockCounter('conversations', () =>
      (detail?.messages ?? []).filter(
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (m: any) => !m.deleted && !m.legal_takedown_ref && msgRender(m).placeholder?.verb === 'block',
      ).length,
    ),
  );
  // Guardian Notify (family-safety.md § Guardian Notify): count each rendered DM
  // message whose guardian floor enforces — a no-op unless content_notify is on. The
  // buffer dedups per message per local day, so re-runs on any thread change are safe.
  $effect(() => {
    for (const msg of detail?.messages ?? []) notifyBuffer.record(msg.message_id, msg.labels);
  });

  // ── display helpers ─────────────────────────────────────────────────
  // The per-rail variant→display switch lives once in shared Rust
  // (`fauna_conversations::TypedAddress::display`, over wasm `typedAddressDisplay`) —
  // the wasm twin of the native FFI `typed_address_display` the other 5 clients
  // consume (conversations.md § Where logic lives). This wrapper only keeps the
  // non-object guard + a non-throwing contract (a malformed address → '', as the
  // old TS switch returned for an unknown variant).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function addrDisplay(a: any): string {
    if (!a || typeof a !== 'object') return String(a ?? '');
    try {
      return typedAddressDisplay(a);
    } catch {
      return '';
    }
  }

  // ── bridged rooms (conversations.md § Where logic lives → The `Bridged`
  // adapter, ruling 2 (a)) ──
  // The glyph is the snapshot's — on a bridged room the one its bridge
  // declared, with the bridge's declared label beside it so two bridges sharing
  // a glyph still read apart. No app-side mapping: both come off the thread.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function protocolIconText(thread: any): string {
    const glyph = sourceGlyphEmoji(thread.glyph);
    return thread.bridge ? `${glyph} ${thread.bridge.label}` : glyph;
  }

  // The bridges serving the account, each by the label it declared — which far
  // networks a typed address may reach (ruling 2 (d): the nest matches an
  // address to its bridge; this is only the list).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let bridgeLabels = $derived(((snap?.bridges ?? []) as any[]).map((b) => b.label).join(', '));

  // A bridged address reads with its bridge's declared label beside it, so the
  // user sees which network the nest resolved it to.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function chipDisplay(a: any): string {
    const shown = addrDisplay(a);
    const bridgeId = a?.Bridged?.bridge_id;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const bridge = bridgeId ? ((snap?.bridges ?? []) as any[]).find((b) => b.id === bridgeId) : null;
    return bridge ? `${shown} · ${bridge.label}` : shown;
  }

  // ── attachments (conversations.md § Attachments → Per-app render lift) ──
  // Per-attachment object-URL cache: built once per `blob_hash` off the bytes the
  // shared `attachmentBytes` loader returns, revoked on destroy — so a reactive
  // re-render doesn't leak a fresh blob URL each tick. `attachmentBytes` returns
  // `undefined` until the bytes are cached (inbound MIME parse / send echo /
  // FaunaMls blob fetch), so a miss isn't cached and the next render retries.
  // The browser twin of linux `message_bubble.rs build_attachment` (paint via
  // `attachment_bytes`, fall back to a generic affordance when not loaded).
  const attachmentUrls = new Map<string, string>();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function attachmentUrl(att: any): string | null {
    const cached = attachmentUrls.get(att.blob_hash);
    // A blob URL outlives the bytes it was made from, so a cached one is only
    // served while the store still holds them: an evicted picture turns back into
    // its placeholder, and the miss below asks the receive loop to fetch it again
    // (conversations.md § Attachments → Retention).
    if (cached && manager?.attachmentResident(att.blob_hash)) return cached;
    if (cached) {
      URL.revokeObjectURL(cached);
      attachmentUrls.delete(att.blob_hash);
    }
    const bytes = manager?.attachmentBytes(att.blob_hash);
    if (!bytes || bytes.length === 0) return null;
    const url = URL.createObjectURL(new Blob([toArrayBufferView(bytes)], { type: att.mime_type || 'application/octet-stream' }));
    attachmentUrls.set(att.blob_hash, url);
    return url;
  }
  onDestroy(() => {
    cleanupActorScope?.();
    for (const u of attachmentUrls.values()) URL.revokeObjectURL(u);
    notifyBuffer.destroy();
  });

  // The `recipient-resolve-status` state→(token, label) map lives in shared Rust
  // (`fauna_conversations::compose::recipient_resolve_status_from_variant` over wasm), so the
  // two pickers below can't drift from each other or from the native apps — the twin
  // `resolveStateAttr`/`resolveStatusText` switches this replaced were exactly that drift.
  // `token` drives `data-state`; a null `label` is idle → empty text. Read only inside the
  // `mode === 'new'` / `addPart` blocks, i.e. once a snapshot (hence wasm) exists.
  let newComposeResolve = $derived(
    recipientResolveStatus(newCompose?.recipient_picker?.resolve_state),
  );
  let addPartResolve = $derived(recipientResolveStatus(addPart?.picker?.resolve_state));

  // The capability-gating contract: render every affordance, and signal the
  // gate via the real `disabled` attribute (the e2e bridge's `get_attr(...,
  // "disabled")` reads `Locator.is_disabled()`, a native-attribute check —
  // it does NOT fall back to `data-disabled`, unlike other attribute names).
  // `data-disabled`/`class:disabled` stay too, for CSS styling.
  // caps null (new-thread compose, no rail yet) = enabled.
  function isGated(supported: boolean | undefined): boolean {
    return !!caps && supported === false;
  }

  // ── mutator glue (every handler refreshes the snapshot after) ────────
  async function run(fn: () => unknown | Promise<unknown>): Promise<void> {
    // Refuse while the wasm ConversationsManager is still building (it arrives
    // async after mount): every handler reaches it as `manager?.`, which would
    // no-op INVISIBLY in that window — a user action silently dropped, the
    // human half of e2e convention 11's "never silently drop a command". This
    // chokepoint guard covers every current and future handler by construction
    // (the same argument feed-refresh-contract.test.ts makes for the feed
    // page's per-site refresh); pinned by manager-gate-contract.test.ts. The
    // message clears on the next successful action (`pageError = ''` below).
    if (!manager) {
      pageError = t.common.still_loading;
      return;
    }
    try {
      pageError = '';
      await fn();
    } catch (e) {
      pageError = e instanceof Error ? e.message : String(e);
    } finally {
      refreshConversations();
      // Persist drafts after every compose mutation (debounced). run() is the
      // page's UI-action chokepoint — the receive poll refreshes directly, never
      // via run() — so this fires on the user's edits but not on inbound delivery
      // (file-sync.md § Drafts Sync). saveDrafts re-uploads idempotently, so a
      // non-compose action (select/sort) at worst re-puts the unchanged blob.
      scheduleDraftSave();
    }
  }

  // List pane.
  function newConversation() { run(() => manager?.startNewConversation()); }
  // Explicit discard of the new-thread composer. The draft now PERSISTS across
  // switching threads (shared ConversationsManager), so an explicit Cancel is the
  // only way (besides Send) to clear it. See conversations.md § Persistence.
  function cancelNewConversation() { run(() => manager?.cancelNewConversation()); }
  // Selecting a thread is what reads it — the shared manager decides that
  // (conversations.md § State & data shape → When a thread is read), so there
  // is no `markRead` here to keep in step.
  function selectThread(id: string) { run(() => manager?.selectThread(id)); }
  function cycleSort() {
    run(() => manager?.setSort(nextSortOrder(snap?.sort)));
  }
  function onSearch(e: Event) {
    const v = (e.target as HTMLInputElement).value;
    run(() => manager?.setSearchQuery(v || undefined));
  }

  // Recipient picker (shared between new-thread compose + add-participant).
  let resolveTimer: ReturnType<typeof setTimeout> | null = null;
  function onRecipientInput(e: Event) {
    const v = (e.target as HTMLInputElement).value;
    if (addPart) manager?.setAddParticipantRecipientInput(v);
    else manager?.setNewThreadRecipientInput(v);
    refreshConversations();
    // Debounced async resolve so the picker reaches a terminal resolve state
    // without an explicit Enter (Playwright fill/type dispatches `input`); the
    // `resolve_recipient` e2e reads the status without sending.
    if (resolveTimer) clearTimeout(resolveTimer);
    resolveTimer = setTimeout(() => {
      run(async () => { await manager?.resolveRecipient(); });
    }, 30);
  }
  function onRecipientKeydown(e: KeyboardEvent) {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    // Resolve, then commit the resolved chip — mirrors linux on_accept.
    run(async () => {
      await manager?.resolveRecipient();
      manager?.acceptCurrentRecipientChip();
    });
  }

  // Compose bar.
  function setBody(v: string) {
    if (mode === 'new') run(() => manager?.setNewThreadBody(v));
    else if (selectedId) run(() => manager?.setComposeBody(selectedId, v));
  }
  function onSubject(e: Event) {
    const v = (e.target as HTMLInputElement).value;
    if (mode === 'new') run(() => manager?.setNewThreadSubject(v));
    else if (selectedId) run(() => manager?.setComposeSubject(selectedId, v));
  }
  function toggleTopic() {
    if (mode === 'new') {
      const cur = newCompose?.subject_draft;
      run(() => manager?.setNewThreadSubject(cur == null ? '' : undefined));
    } else if (selectedId) {
      run(() => manager?.toggleTopic(selectedId));
    }
  }
  function send() {
    if (mode === 'new') run(async () => { await manager?.sendNewThread(); });
    else if (selectedId) run(async () => { await manager?.send(selectedId); });
  }
  // `attachment-button` (a real `<input type="file">`, not a dialog-opening
  // button — Playwright drives it directly via `set_input_files`, the same
  // shape `compose-file` on the feed composer already uses). Stage each picked
  // file's bytes on the active compose draft — new-thread vs reply mirrors
  // every other compose mutator's `mode` dispatch above (`docs/goal/ui/
  // conversations.md` § Attachments: `attachment-button` row).
  async function stageAttachments(fileList: FileList | null) {
    if (!fileList || fileList.length === 0) return;
    const files = Array.from(fileList);
    await run(async () => {
      for (const file of files) {
        const buf = await file.arrayBuffer();
        const bytes = new Uint8Array(buf);
        const mimeType = file.type || 'application/octet-stream';
        if (mode === 'new') {
          manager?.addNewThreadAttachment(file.name, mimeType, bytes);
        } else if (selectedId) {
          manager?.addAttachment(selectedId, file.name, mimeType, bytes);
        }
      }
    });
  }
  // `dm-reply-button` seeds the reply To line with the message's sender;
  // `dm-reply-all-button` seeds every participant but self. Both go through the
  // shared `start_reply` (sets reply_to + seeds reply_recipients on mail), then
  // the To line is editable. Off mail, start_reply just sets reply_to.
  function replyTo(msgId: string) {
    if (selectedId) run(() => manager?.startReply(selectedId, msgId, false));
  }
  function replyAll(msgId: string) {
    if (selectedId) run(() => manager?.startReply(selectedId, msgId, true));
  }
  // Remote-image reveal is the MANAGER's (render-model.md § D3): `revealRemoteImages`
  // flips the manager-owned reveal set + re-emits, so the next `threadDetail` carries
  // `RemoteImage.revealed: true` for that message and the bubble re-paints it in fetch
  // mode (`<img src>`). The page keeps no reveal dictionary of its own — the document's
  // per-block `revealed` is the single source of truth.
  function cancelReply() {
    if (selectedId) run(() => manager?.setReplyTo(selectedId, undefined));
  }
  // Editable reply "To" line (mail only — supports_recipient_selection).
  function addReplyRecipient() {
    const v = replyRecipientInput.trim();
    if (v && selectedId) {
      run(() => manager?.addReplyRecipient(selectedId, v));
      replyRecipientInput = '';
    }
  }
  function onReplyRecipientKeydown(e: KeyboardEvent) {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    addReplyRecipient();
  }
  function removeReplyRecipient(addr: string) {
    if (selectedId) run(() => manager?.removeReplyRecipient(selectedId, addr));
  }
  function removeComposeAttachment(index: number) {
    if (mode === 'new') run(() => manager?.removeNewThreadAttachment(index));
    else if (selectedId) run(() => manager?.removeAttachment(selectedId, index));
  }

  // Add-participant overlay.
  function openAddParticipant() {
    if (selectedId) run(() => manager?.openAddParticipant(selectedId));
  }
  function confirmAddParticipant() { run(async () => { await manager?.confirmAddParticipant(); }); }
  function cancelAddParticipant() { run(() => manager?.cancelAddParticipant()); }

  // `recipient-picker-class` — the class of the room the new-thread picker is
  // about to create, `null` before the first chip is committed. Derived in
  // shared Rust from the committed chips and the home-nest choice
  // (`fauna_conversations::room::prospective_room_class`), exactly as every
  // other app's picker derives it.
  let newComposeRoomClass = $derived.by(() => {
    const picker = newCompose?.recipient_picker;
    if (!picker) return null;
    return roomProspectiveClass(picker.chips ?? [], !!picker.include_home_nest);
  });

  // The two pickers' staged values, and the per-member row states the editor
  // paints. Every one of them is READ BACK OUT OF THE DRAFT after each staging
  // gesture rather than tracked alongside it, so the draft stays the single
  // authority — the at-most-one-staged hand-over rule in particular is only
  // ever observed, never re-implemented (`RoomSettingsDraft`; priority #2).
  // The two `join_rule` / `history_policy` field reads are the same reads a
  // UniFFI app makes on the record; the token mapping is shared Rust's.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let roomDraftJoinRule = $derived((roomDraft as any)?.join_rule ?? 'Invite');
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let roomDraftHistoryPolicy = $derived((roomDraft as any)?.history_policy ?? 'None');
  let roomSettingsRows = $derived.by(() => {
    const id = selectedId;
    const draft = roomDraft;
    const mgr = manager;
    if (!draft || !id || !mgr) return [];
    return (detail?.participant_displays ?? []).map((_: unknown, i: number) => ({
      admin: mgr.roomSettingsAdminAt(draft, id, i),
      transfer: mgr.roomSettingsTransferStagedAt(draft, id, i),
      eligible: mgr.roomSettingsIsEligible(draft, id, i),
    }));
  });

  // ── The room policy editor (`room_settings` sub-page) ────────────────
  //
  // A PAINTER and nothing more (`conversation-rooms.md` § Implementation
  // status today): the seed, which rows either control may act on, the
  // at-most-one-staged hand-over rule, the diff and the commit order all live
  // in `fauna_conversations::RoomSettingsDraft` +
  // `ConversationsManager::apply_room_settings`, reached over the wasm twins.
  // Nothing below re-derives a role rule.
  function openRoomSettings() {
    if (!selectedId) return;
    // The same refusal `run()` makes, for the same reason: seeding is a
    // snapshot READ so it needs no `run()` round trip, but a `manager?.` here
    // would no-op INVISIBLY during the manager's build window — the user
    // presses the editor's door and nothing at all happens (convention 11).
    if (!manager) {
      pageError = t.common.still_loading;
      return;
    }
    // A thread with no policy to edit seeds `null`, and the editor stays shut.
    roomDraft = manager.roomSettingsSeed(selectedId) ?? null;
    roomSettingsOpen = roomDraft !== null;
  }
  function cancelRoomSettings() {
    roomSettingsOpen = false;
    roomDraft = null;
  }
  // The four staging gestures below carry no such guard on purpose: they are
  // reachable only from inside an OPEN editor, and the editor opens only where
  // `openRoomSettings` found a manager — so `manager?.` here cannot be the
  // invisible no-op the guard above exists for.
  function stageJoinRule(e: Event) {
    const token = (e.target as HTMLSelectElement).value;
    if (roomDraft) roomDraft = manager?.roomSettingsSetJoinRule(roomDraft, token) ?? roomDraft;
  }
  function stageHistoryPolicy(e: Event) {
    const token = (e.target as HTMLSelectElement).value;
    if (roomDraft)
      roomDraft = manager?.roomSettingsSetHistoryPolicy(roomDraft, token) ?? roomDraft;
  }
  function stageAdmin(i: number) {
    const id = selectedId;
    if (roomDraft && id) roomDraft = manager?.roomSettingsToggleAdmin(roomDraft, id, i) ?? roomDraft;
  }
  function stageTransfer(i: number) {
    const id = selectedId;
    if (roomDraft && id)
      roomDraft = manager?.roomSettingsToggleTransfer(roomDraft, id, i) ?? roomDraft;
  }
  // Save is ONE call. The editor must OUTLIVE it — `apply_room_settings` stops
  // at the first refusal and returns whether every edit landed, and the
  // contract is "closes only when all landed" (`ui/conversations.md`
  // § Element IDs), so a `false` leaves the editor open over the page error
  // the refusing call already painted.
  function saveRoomSettings() {
    const id = selectedId;
    const draft = roomDraft;
    if (!id || !draft) return;
    run(async () => {
      const edits = manager?.roomSettingsEdits(draft, id);
      const allLanded = await manager?.applyRoomSettings(id, edits);
      if (allLanded) {
        roomSettingsOpen = false;
        roomDraft = null;
      }
    });
  }

  // Rename overlay.
  function openRename() {
    renameValue = detail?.label ?? '';
    renaming = true;
  }
  function confirmRename() {
    const id = selectedId;
    const v = renameValue;
    renaming = false;
    if (id) run(async () => { await manager?.renameThread(id, v); });
  }
  function cancelRename() { renaming = false; }

  // ── reactions & message delete (conversations.md § Reactions & message delete) ──
  // FaunaMls-only, capability-gated like every other affordance (never branch on
  // rail). The DOM twin of windows `DmMessageBubble` / apple's inline @State menu.
  //
  // The fixed quick-set, in the canonical order shared with every app (the e2e
  // indexes `dm-reaction-option` in THIS order) — READ FROM SHARED RUST, never re-typed
  // here: `fauna_conversations::QUICKSET_EMOJIS` over the `quicksetEmojis` wasm face, the
  // twin of the UniFFI face apple and android read (priority #2/#4 — four hand-kept copies
  // of an order that is a cross-app contract had nothing to catch them drifting; windows'
  // is the last one left, and only because its C# needs a Windows machine to compile).
  // Filled in `onMount` after `ensureWasm()`, because `wasm()` throws before then; the
  // menu that renders these is unreachable until the snapshot exists, so the empty
  // starting value is never painted.
  //
  // `MORE_GRID_EMOJI` stays web-local: it is the fuller "more" grid, and web has no native
  // emoji picker, so a built grid is the one sanctioned per-platform divergence (§ Rendering
  // / picker glue), same toggle path; its buttons carry NO `dm-reaction-option` id so they
  // never inflate the fixed-6 quick-set count the e2e relies on.
  let quickSetEmoji = $state<string[]>([]);
  const MORE_GRID_EMOJI = [
    '👍', '❤️', '😂', '😮', '😢', '🙏', '🎉', '🔥', '👏', '🙌',
    '😍', '🤔', '😎', '😡', '👀', '✅', '❌', '💯', '🚀', '⭐',
  ];

  // Per-bubble flyout state — client-local UI (the manager has no "menu open" flag),
  // keyed by `message_id` so a snapshot refresh (a fresh object each tick) doesn't
  // reset it. Only one menu open at a time (the inline `@State` shape apple uses).
  let openActionsFor = $state<string | null>(null);
  let openMoreFor = $state<string | null>(null);
  let openDeleteConfirmFor = $state<string | null>(null);

  function closeMessageMenus() {
    openActionsFor = null;
    openMoreFor = null;
    openDeleteConfirmFor = null;
  }
  function toggleMessageActions(msgId: string) {
    if (openActionsFor === msgId) { closeMessageMenus(); return; }
    closeMessageMenus();
    openActionsFor = msgId;
  }
  function openMoreReactions(msgId: string) {
    closeMessageMenus();
    openMoreFor = msgId;
  }
  function openDeleteConfirm(msgId: string) {
    closeMessageMenus();
    openDeleteConfirmFor = msgId;
  }
  // Pick an emoji (quick-set or grid) → toggle the reaction through the shared
  // manager (resolves Add vs Remove against self for that emoji), then close. The
  // optimistic aggregate update + the wire Reaction op both live in Rust; the page
  // only supplies the picked emoji string (§ Rendering / picker glue).
  function pickReaction(msgId: string, emoji: string) {
    const id = selectedId;
    closeMessageMenus();
    if (id) run(async () => { await manager?.toggleReaction(id, msgId, emoji); });
  }
  // Tap an existing pill to toggle that reaction off/on — the same shared path.
  function togglePill(msgId: string, emoji: string) {
    const id = selectedId;
    if (id) run(async () => { await manager?.toggleReaction(id, msgId, emoji); });
  }
  // Confirm sender-only delete → the manager optimistically marks the target
  // `deleted` (the wire `Delete` op is best-effort after) and the bubble re-paints
  // the localized tombstone. Manager rejects a non-own target (defence-in-depth).
  function confirmDelete(msgId: string) {
    const id = selectedId;
    closeMessageMenus();
    if (id) run(async () => { await manager?.deleteMessage(id, msgId); });
  }
  // Mark-as-spam — the live `Insert` consumer (mail-spam.md § Wire shapes). Fire-and-forget:
  // trains the sealed tier-1 model over the retained decrypted body + writes a sealed
  // training-history row (renders on the mail-spam page with per-row undo); silently
  // no-ops when the nest lacks `spam-model-sealed-at-rest` or mail isn't enabled — a
  // conversation message is client-only encrypted content the nest cannot read, so there
  // is no server-train fallback (unlike the moderation-queue server-row correction).
  function markAsSpam(msgId: string, body: string, subjectLine: string | undefined) {
    closeMessageMenus();
    run(async () => { await markMessageSpam(msgId, body, subjectLine ?? ''); });
  }
</script>

<div class="conversations">
  <!-- ── list pane ──────────────────────────────────────────────── -->
  <section class="list-pane">
    <div class="list-top">
      <h1 data-testid={IDS.PAGE_HEADING}>{t.conversations.list.title}</h1>
      <button data-testid={IDS.NEW_CONVERSATION_BUTTON} class="icon-btn" title={t.conversations.list.new_conversation} disabled={!manager} onclick={newConversation}>+</button>
      <button data-testid={IDS.CONVERSATION_SORT} class="icon-btn" title={t.conversations.list.sort} onclick={cycleSort}>{'↕'}</button>
    </div>
    <input
      data-testid={IDS.CONVERSATION_SEARCH_BOX}
      class="search"
      type="text"
      placeholder={t.conversations.list.search_placeholder}
      value={snap?.search_query ?? ''}
      oninput={onSearch}
    />

    {#if !ready}
      <p class="muted">{t.common.loading}</p>
    {:else if !$identity}
      <p class="muted">{t.common.identity_required}</p>
    {:else if threads.length === 0}
      <p class="muted">{t.conversations.list.no_conversations}</p>
    {/if}

    <div class="rows">
      <!-- eslint-disable-next-line -->
      {#each threads as thread, i (thread.thread_id)}
        <button
          data-testid={IDS.CONVERSATION_ITEM}
          class="row"
          class:selected={thread.thread_id === selectedId}
          onclick={() => selectThread(thread.thread_id)}
        >
          {#if thread.unread_count > 0}
            <span data-testid={IDS.DM_UNREAD_INDICATOR} class="unread-dot"></span>
          {/if}
          <span data-testid={IDS.PROTOCOL_ICON} class="protocol-icon" data-bridge={thread.bridge?.id}>{protocolIconText(thread)}</span>
          <!-- The family gate's marker, only while the nest reports one for
               this room's peer (`family-safety.md` § The bridge-DM gate → *App
               affordance*): computed there, painted here, and the row still
               opens. -->
          {#if thread.guardian_state}
            <span
              data-testid={IDS.CONVERSATION_GUARDIAN_STATE}
              class="guardian-state"
              data-state={guardianStateAttrToken(thread.guardian_state)}
              >{guardianStateLabel(thread.guardian_state)}</span
            >
          {/if}
          <span class="row-text">
            <!-- Top: participant label (no testid). Bottom: the snippet preview
                 carries `dm-subject` (ui.yaml "subject display in list"), matching
                 linux list.rs — the snippet is markdown-stripped to plaintext in
                 shared `summarize`, so `**bold**` previews as `bold`, never raw. -->
            <span class="row-label">{resolveLocalized(threadLabelDisplay(thread.label))}</span>
            <span data-testid={IDS.DM_SUBJECT} class="row-snippet">{thread.snippet}</span>
          </span>
          <!-- Shared contextual last-activity formatter (today → local clock /
               Yesterday / weekday / older → date). Plain row text, no testid —
               matches linux/windows (value-formatting.md § Conversation timestamp;
               conversations.md list-row "clients must not hand-roll it"). -->
          <span class="row-time">{conversationTimestamp(thread.last_activity_ms)}</span>
        </button>
      {/each}
    </div>
  </section>

  <!-- ── detail pane ────────────────────────────────────────────── -->
  <section class="detail-pane">
    {#if displayError}<p data-testid={IDS.ERROR_MESSAGE} class="error">{displayError}</p>{/if}

    {#if mode === 'empty'}
      <p class="muted hint">{t.conversations.list.select_conversation}</p>
    {/if}

    {#if mode === 'thread' && detail}
      <!-- thread header -->
      <div data-testid={IDS.THREAD_HEADER} class="thread-header">
        <span class="thread-title">{resolveLocalized(threadLabelDisplay(detail.label))}</span>
        <span data-testid={IDS.PROTOCOL_ICON} class="protocol-icon" data-bridge={detail.bridge?.id}>{protocolIconText(detail)}</span>
        <!-- The room's class statement, read off the projected room and never
             computed here — the class is a pure function of the member set,
             derived in shared Rust (`conversation-rooms.md` § The three
             classes). ABSENT (not hidden) where the rail models no room,
             which is the goal doc's own word for it and what a driver's
             subtree prune reads.

             The driver-facing token rides `data-class`, not the CSS `class`
             attribute the cross-app contract names: on web that name is
             already the style class list. The bridge resolves `class` to
             `data-class` for exactly this reason
             (`tests/e2e-unified/web-bridge/server.py`'s `/element/attr` —
             the prefixed name wins over a same-named real attribute), so
             `get_attr("thread-room-class", "class")` reads the token here as
             it does on every other app. -->
        {#if detail.room}
          <span
            data-testid={IDS.THREAD_ROOM_CLASS}
            class="room-class"
            data-class={roomClassAttrToken(detail.room.class)}
            >{roomClassLabel(detail.room.class)}</span
          >
        {/if}
        <!-- The family gate's marker in the detail, the row's twin — the
             thread below it stays fully readable. -->
        {#if detail.guardian_state}
          <span
            data-testid={IDS.CONVERSATION_GUARDIAN_STATE}
            class="guardian-state"
            data-state={guardianStateAttrToken(detail.guardian_state)}
            >{guardianStateLabel(detail.guardian_state)}</span
          >
        {/if}
        <div class="members">
          <!-- eslint-disable-next-line -->
          {#each detail.participant_displays ?? [] as disp, i}
            <!-- The post-succession review pair renders INSIDE the chip, never
                 beside it (succession-aftermath.md § Propagation → MLS groups).
                 The pair renders only on FLAGGED members, so a flat list would
                 index marks 0..n over chips 0..m: a driver reading
                 `thread-member-unattested-mark[0]` would get the first flagged
                 member while `thread-member-chip[0]` is the first member, and
                 the two would silently disagree about who is being asked about.
                 Nesting makes a scoped read resolve as a descendant match.
                 ⚠ Remove is deliberately NOT re-rendered as its own button:
                 the chip itself IS it, exactly as `nest-trust-grant-revoke` is
                 the grant plane's Remove half. Gated on
                 `supports_membership_change` like tui's own branch — Keep
                 beside a chip that cannot remove anyone is half a ratified
                 pair. Two gates, meaning different things, exactly as linux's
                 header does it. The rail's `supports_membership_change`
                 decides whether the chip is a control at all: where the rail
                 models no membership (mail) it is a label — no role, no tab
                 stop, no handler. The viewer's role decides whether that
                 control is LIVE — the roles table applied in shared Rust
                 (`RoomSnapshot::gate`), never re-derived here
                 (`conversation-rooms.md` § Roles and authorization) — and a
                 control the role withholds is GREYED, never demoted to a label
                 (`ui/conversations.md` § Architectural rules 5):
                 `role="button"` + `aria-disabled`, which is also what a driver
                 reads as disabled. A bare span here read as ENABLED to every
                 driver and swallowed the click. -->
            {@const membershipRail = !!detail.capabilities?.supports_membership_change}
            {@const removable = membershipRail && !!detail.capabilities?.can_remove_members}
            <!-- The member's role on a governed room: the `role` attribute a
                 driver reads off the chip, and the localized owner/admin mark
                 in its text. `null` on a policy-less room and on every non-room
                 thread, where the chip is the bare display name. Both come
                 from shared Rust — `role` is index-parallel with the chips by
                 construction — `RoomSnapshot::members` is index-parallel with
                 `ThreadDetail::participants`, and so with these chips. -->
            {@const memberRole = (detail.room?.members ?? [])[i]?.role ?? null}
            <!-- The role and the tab stop are applied together or not at all,
                 so a chip that carries `tabindex` is always `role="button"`.
                 svelte-check reads the conditional `role` as unresolvable and
                 judges the span noninteractive on that basis alone. -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <span
              data-testid={IDS.THREAD_MEMBER_CHIP}
              class="chip"
              class:removable
              class:greyed={membershipRail && !removable}
              data-role={memberRole ? roomRoleAttrToken(memberRole) : undefined}
              role={membershipRail ? 'button' : undefined}
              aria-disabled={membershipRail && !removable ? 'true' : undefined}
              tabindex={membershipRail ? (removable ? 0 : -1) : undefined}
              onclick={removable ? () => removeMember(i) : undefined}
              onkeydown={removable
                ? (e: KeyboardEvent) => {
                    if (e.key !== 'Enter' && e.key !== ' ') return;
                    e.preventDefault();
                    removeMember(i);
                  }
                : undefined}
            >
              {roomMemberChipText(disp, memberRole)}
              {#if removable && memberMarks[i]}
                <span data-testid={IDS.THREAD_MEMBER_UNATTESTED_MARK} class="chip-mark"
                  >{t.conversations.detail.member_unattested_mark}</span
                >
                <!-- Keep sits INSIDE the chip (so a scoped read resolves as a
                     descendant match) and the chip is now Remove, so Keep must
                     stop the click from reaching it — otherwise answering
                     "keep this member" would also evict them, which is the
                     exact inverse of the verdict pressed. -->
                <button
                  data-testid={IDS.THREAD_MEMBER_KEEP_BUTTON}
                  class="chip-keep"
                  onclick={(e) => {
                    e.stopPropagation();
                    keepMember(memberMarks[i] as string);
                  }}
                  >{t.conversations.detail.member_keep}</button
                >
              {/if}
            </span>
          {/each}
        </div>
        <!-- Present off the RAIL's `supports_membership_change` (does this
             rail model membership at all), then GREYED — never hidden — off
             the viewer's role permitting an invite (`ui/conversations.md`
             § Architectural rules 5). The role half used to be missing
             entirely: every app gated add/remove on the rail capability
             alone, so a plain member of a governed room was offered an Add
             the room would refuse. linux fixed it 2026-09-09; this is web's
             half of the same correction. -->
        {#if detail.capabilities?.supports_membership_change}
          <button
            data-testid={IDS.THREAD_ADD_PARTICIPANT_BUTTON}
            class="hdr-btn"
            disabled={!detail.capabilities?.can_invite}
            onclick={openAddParticipant}
          >
            {t.conversations.unified.thread_add_participant}
          </button>
        {/if}
        <!-- The policy editor's door: painted whenever the thread is a room,
             greyed unless the viewer may set the policy — a policy-less room greys
             it too, having nothing to edit (`ui/conversations.md` § Element
             IDs). Absent, like the class statement, where the rail models no
             room. -->
        {#if detail.room}
          <button
            data-testid={IDS.THREAD_ROOM_SETTINGS_BUTTON}
            class="hdr-btn"
            disabled={!detail.capabilities?.can_set_policy}
            onclick={openRoomSettings}
          >
            {t.conversations.unified.thread_room_settings}
          </button>
        {/if}
        {#if detail.capabilities?.supports_rename}
          <button data-testid={IDS.THREAD_RENAME_BUTTON} class="hdr-btn" onclick={openRename}>
            {t.conversations.unified.thread_rename}
          </button>
        {/if}
      </div>

      <!-- messages stream -->
      <div class="messages">
        <!-- eslint-disable-next-line -->
        {#each detail.messages ?? [] as msg, i (msg.message_id)}
          {@const quote = quotedMessageBlock(msg.document)}
          {@const showReactions = !!caps?.supports_reactions}
          {@const showDelete = !!caps?.supports_message_delete && !!msg.is_own}
          {@const showFlagSpam = !msg.is_own}
          {@const showActions = showReactions || showDelete || showFlagSpam}
          {#if msg.subject_line}
            <div data-testid={IDS.SUBJECT_DIVIDER} class="subject-divider">{msg.subject_line}</div>
          {/if}
          <div data-testid={IDS.DM_MESSAGE_BUBBLE} class="bubble">
            {#if msg.deleted}
              <!-- Tombstone (conversations.md § Reactions & message delete): a deleted
                   message collapses to a localized placeholder — body/attachments/quote/
                   reactions/⋯-actions all hidden. Mirrors windows `DmMessageBubble`'s
                   deleted bind + apple's early-return. -->
              <div data-testid={IDS.DM_MESSAGE_DELETED} class="message-deleted">{t.conversations.detail.message_deleted}</div>
            {:else if msg.legal_takedown_ref}
              <!-- Legal-takedown tombstone (moderation.md § Categories & enforcement item 1):
                   the nest withheld the sealed envelope under a legal obligation, so the bubble
                   collapses (like `deleted`) to the shared localized tombstone rendered in place
                   of the (empty) body — never a blank/failed-decrypt bubble. Presentation only,
                   no dedicated e2e id (like the post quoted-post tombstone / ContentLabelBadge;
                   a new id would need ui.yaml approval first, § UI Consistency A). Twin of
                   linux `message_bubble.rs`'s early-return + the post `QuotedPost.svelte` paint. -->
              <div class="message-deleted message-legal-takedown">{resolveLocalized(legalTakedownTombstone(msg.legal_takedown_ref))}</div>
            {:else if regionPlaceholderMsg(msg)}
              <!-- A REGION verdict (`region-blocking.md` § The blocked render): the
                   placeholder in place of the body, ahead of the family arm (tui's
                   `conversations/mod.rs` order). -->
              <RegionPlaceholder
                class="message-deleted"
                placeholder={regionPlaceholderMsg(msg)!}
                onreveal={() => revealContent(msg.message_id)}
              />
            {:else if isContentBlockedMsg(msg)}
              <!-- Content-policy BLOCK (a guardian floor) — a policy-naming notice
                   in place of the (sealed) body, NO reveal (family-safety.md
                   § Content policy). Checked AHEAD of the muted arm (absolute) — the
                   twin of linux `message_bubble.rs`'s early-return; renders the one
                   ui.yaml ID this pillar surfaces on the bubble. -->
              <div data-testid={IDS.CONTENT_POLICY_BLOCKED_NOTICE} class="message-deleted message-content-blocked">{t.family.content_blocked_notice}</div>
            {:else if isMuted(msg) && !revealedMuted.has(msg.message_id)}
              <!-- Muted-keyword collapse (moderation.md § Muted keywords;
                   content-moderation-and-ranking.md § Q3 — Option A, per-app,
                   at-render, session-local reveal): a decrypted message whose body
                   matches the user's muted list is collapsed behind a placeholder
                   with a one-tap reveal — a hide/collapse, NOT a spam-queue flag
                   (does not touch LocalDetectionStore / merge_queue). Mirrors the
                   deleted/legal-takedown tombstone early-returns above and the
                   `load-remote-content-button` reveal gesture below. -->
              <div data-testid={IDS.DM_MESSAGE_MUTED} class="message-deleted message-muted">{t.conversations.detail.muted_word}</div>
              <button
                data-testid={IDS.DM_MESSAGE_MUTED_REVEAL_BUTTON}
                class="reply-btn load-remote-btn"
                onclick={() => revealMuted(msg.message_id)}
              >{t.conversations.detail.muted_reveal}</button>
            {:else if isContentCollapsedMsg(msg)}
              <!-- Content-policy COLLAPSE (a viewer's own threshold, or a guardian
                   `collapse` floor) — a one-tap reveal, session-local; the floor
                   persists. Presentation only, no test id (v1 e2e drives the block
                   case; linux `build_content_collapse` precedent). -->
              <div class="message-deleted message-content-collapsed">{t.family.content_collapsed_notice}</div>
              <button
                class="reply-btn load-remote-btn"
                onclick={() => revealContent(msg.message_id)}
              >{t.family.content_reveal_button}</button>
            {:else}
            <div class="bubble-head">
              <span data-testid={IDS.DM_SENDER} class="sender">{msg.sender_display || addrDisplay(msg.sender)}</span>
              {#if msg.badges?.encrypted}<span data-testid={IDS.ENCRYPTED_BADGE} class="badge">{t.conversations.compose.encrypted}</span>{/if}
              {#if msg.badges?.signed}<span data-testid={IDS.SIGNED_BADGE} class="badge">{t.conversations.message.signed}</span>{/if}
              {#if msg.badges?.verified}<span data-testid={IDS.VERIFIED_BADGE} class="badge">{t.common.verified}</span>{/if}
              <!-- Per-message timestamp — the SAME shared contextual bucketer the list
                   row uses (today → local clock / Yesterday / weekday / older → date;
                   value-formatting.md § Conversation timestamp). Unifies with linux/apple's
                   bubble timestamp and adds it where web/windows/android showed none
                   (render-model.md § D5-adjacent; priorities #1/#4). -->
              <span data-testid={IDS.DM_MESSAGE_TIMESTAMP} class="bubble-time">{conversationTimestamp(msg.timestamp_ms)}</span>
              <button data-testid={IDS.DM_REPLY_BUTTON} class="reply-btn" onclick={() => replyTo(msg.message_id)}>{t.common.reply}</button>
              {#if caps?.supports_recipient_selection}
                <button data-testid={IDS.DM_REPLY_ALL_BUTTON} class="reply-btn" onclick={() => replyAll(msg.message_id)}>{t.conversations.unified.reply_all}</button>
              {/if}
              <!-- Per-bubble ⋯ overflow — shown iff ≥1 action is available (react OR
                   own-deletable), keeping mail bubbles clean. Capability-gated, never on
                   rail (§ Architectural rules). Opens the inline dm-message-actions-menu. -->
              {#if showActions}
                <button
                  data-testid={IDS.DM_MESSAGE_ACTIONS_BUTTON}
                  class="actions-btn"
                  title={t.conversations.detail.message_actions}
                  aria-haspopup="menu"
                  aria-expanded={openActionsFor === msg.message_id}
                  onclick={() => toggleMessageActions(msg.message_id)}
                >⋯</button>
              {/if}
            </div>
            <!-- Inline ⋯ actions menu: the 6 quick-set dm-reaction-option (gated
                 supports_reactions) + dm-reaction-more-button, then the own-only
                 dm-message-delete-button. Inline (not an absolute flyout) so every id
                 attaches to a real element the Playwright driver counts — the apple
                 inline @State pattern. -->
            {#if openActionsFor === msg.message_id}
              <div data-testid={IDS.DM_MESSAGE_ACTIONS_MENU} class="actions-menu">
                {#if showReactions}
                  <!-- eslint-disable-next-line -->
                  {#each quickSetEmoji as emoji, qi}
                    <button data-testid={IDS.DM_REACTION_OPTION} class="reaction-option" title={emoji} onclick={() => pickReaction(msg.message_id, emoji)}>{emoji}</button>
                  {/each}
                  <button data-testid={IDS.DM_REACTION_MORE_BUTTON} class="reaction-more" onclick={() => openMoreReactions(msg.message_id)}>{t.conversations.detail.more_reactions}</button>
                {/if}
                {#if showDelete}
                  <button data-testid={IDS.DM_MESSAGE_DELETE_BUTTON} class="delete-option" onclick={() => openDeleteConfirm(msg.message_id)}>{t.conversations.detail.delete_message}</button>
                {/if}
                {#if showFlagSpam}
                  <button data-testid={IDS.DM_MESSAGE_MARK_AS_SPAM_BUTTON} class="delete-option" onclick={() => markAsSpam(msg.message_id, msg.body, msg.subject_line)}>{t.conversations.detail.mark_as_spam}</button>
                {/if}
              </div>
            {/if}
            <!-- The fuller "more" emoji grid (the only sanctioned web divergence — no
                 native emoji picker). Plain buttons WITHOUT the dm-reaction-option id so
                 they never inflate the fixed-6 quick-set count; same toggle path. -->
            {#if openMoreFor === msg.message_id}
              <div class="more-grid">
                <!-- eslint-disable-next-line -->
                {#each MORE_GRID_EMOJI as emoji, mi}
                  <button class="grid-emoji" title={emoji} aria-label={emoji} onclick={() => pickReaction(msg.message_id, emoji)}>{emoji}</button>
                {/each}
              </div>
            {/if}
            <!-- Delete-confirm step (destructive-action gate): a title + the single
                 dm-message-delete-confirm-button. -->
            {#if openDeleteConfirmFor === msg.message_id}
              <div class="delete-confirm">
                <span class="confirm-title">{t.conversations.detail.delete_message_confirm_title}</span>
                <button data-testid={IDS.DM_MESSAGE_DELETE_CONFIRM_BUTTON} class="delete-confirm-btn" onclick={() => confirmDelete(msg.message_id)}>{t.conversations.detail.delete_message_confirm}</button>
              </div>
            {/if}
            <!-- Walk the shared `RenderDocument` the conversations manager already built
                 (markdown / plaintext / inbound-HTML all collapse to one typed node tree —
                 render-model.md § D1); the body is no longer re-parsed at render time. The
                 DOM twin of linux `views/conversations/document.rs`.
                 ⚠ Inbound bodies are UNTRUSTED — remote `![]()` images render BLOCKED
                 (`<img data-remote-src… class="blocked-remote-image">`, no `src` → no network
                 fetch; html-mail.md § Security & privacy). The body paints each remote
                 image from its OWN `RemoteImage.revealed` field — the manager projects the
                 reveal set onto the document (render-model.md § D3); no per-call override. -->
            <!-- In-bubble reply-quote (render-model.md § D2 `QuotedMessage`): the manager
                 folds it into the document (prepended) when this message replies to a parent
                 loaded in the thread; hidden otherwise. Painted as a card ABOVE the body (its
                 own `dm-message-quote` element); the body `{@html}` no-ops the block. Author +
                 a parent snippet clamped to ≤ 2 lines (CSS line-clamp). DOM twin of linux
                 `document.rs build_reply_quote`. -->
            {#if quote}
              <div data-testid={IDS.DM_MESSAGE_QUOTE} class="dm-message-quote">
                <span class="quote-author">{quote.author_display}</span>
                <span class="quote-snippet">{quote.snippet}</span>
              </div>
            {/if}
            <!-- The message's content label (moderation.md § Per-row badge data path):
                 the highest-confidence of its `labels`, through the one shared pick the
                 feed card uses; an unlabeled message paints none. -->
            {#if contentLabelBadgeFor(msg.labels)}
              <ContentLabelBadge label={contentLabelBadgeFor(msg.labels) ?? ''} />
            {/if}
            <div data-testid={IDS.DM_MESSAGE_TEXT} class="bubble-body">
              {@html documentToHtml(msg.document)}
            </div>
            {#if documentHasBlockedRemoteImages(msg.document)}
              <button
                data-testid={IDS.LOAD_REMOTE_CONTENT_BUTTON}
                class="reply-btn load-remote-btn"
                onclick={() => { manager?.revealRemoteImages(msg.message_id); refreshConversations(); }}
              >{t.conversations.detail.load_remote_content}</button>
            {/if}
            <!-- Link-preview cards — the `LinkPreview` embed blocks the shared producer
                 appends after a bare-url paragraph (render-model.md § D4). The SAME
                 `link-preview-card` component the feed paints; the og:image loads from the
                 user's own nest blob, gated on the shared `revealed` flag (so the
                 `load-remote-content-button` above reveals body images + og:image together).
                 The fire-once resolve runs in the `$effect` above. -->
            <!-- eslint-disable-next-line -->
            {#each previewCards(msg.document) as lp (lp.url)}
              <LinkPreviewCard
                url={lp.url}
                title={lp.title}
                description={lp.description}
                imageUrl={lp.revealed && lp.image_hash ? previewBlobUrl(lp.image_hash) : null}
              />
            {/each}
            <!-- Attachments — the `Attachment` embed blocks the manager appended after the
                 body (render-model.md § D2: `attachmentBlocks(msg.document)`, no longer the
                 sibling `msg.attachments` field). Image: paint the bytes the shared
                 `attachmentBytes(blob_hash)` loader returns as an <img> from a blob URL (a
                 reactive blob URL `documentToHtml`'s `{@html}` can't produce, so it stays in
                 the template); until the bytes are loaded (a FaunaMls nest blob not yet
                 GET+decrypted, or evicted with nowhere to refill from) the DECLARED
                 placeholder under the same id — filename and size (conversations.md
                 § Attachments → Retention). `data-state` answers painted / placeholder.
                 Non-image: a file affordance carrying the filename + size, a download link
                 once the bytes resolve. Mirrors linux `document.rs build_attachment`. -->
            <!-- eslint-disable-next-line -->
            {#each attachmentBlocks(msg.document) as att, ai (att.blob_hash + ':' + ai)}
              {#if att.is_image}
                {@const url = attachmentUrl(att)}
                {#if url}
                  <img data-testid={IDS.DM_ATTACHMENT_IMAGE} data-state="painted" class="dm-attachment dm-attachment-image" src={url} alt={att.filename} title={att.filename} />
                {:else}
                  <span data-testid={IDS.DM_ATTACHMENT_IMAGE} data-state="placeholder" class="dm-attachment dm-attachment-file" title={att.filename}>🖼 {att.filename} · {byteSize(att.size_bytes)}</span>
                {/if}
              {:else}
                {@const url = attachmentUrl(att)}
                {#if url}
                  <a data-testid={IDS.DM_ATTACHMENT_FILE} class="dm-attachment dm-attachment-file" href={url} download={att.filename} title={att.filename}>📎 {att.filename} · {byteSize(att.size_bytes)}</a>
                {:else}
                  <span data-testid={IDS.DM_ATTACHMENT_FILE} class="dm-attachment dm-attachment-file" title={att.filename}>📎 {att.filename} · {byteSize(att.size_bytes)}</span>
                {/if}
              {/if}
            {/each}
            <!-- Aggregated reactions under the bubble (manager-folded
                 MessageSnapshot.reactions): emoji + count, own highlighted, tap-to-toggle
                 the same shared path. dm-reaction-pill is indexed (conversations.md
                 § Reactions & message delete). The pill round-trip is real-FaunaMls-only
                 (the mock backend sets no me_actor → toggle_reaction no-ops). -->
            {#if showReactions && (msg.reactions?.length ?? 0) > 0}
              <div class="reaction-pills">
                <!-- eslint-disable-next-line -->
                {#each msg.reactions as g, gi}
                  <button data-testid={IDS.DM_REACTION_PILL} class="reaction-pill" class:mine={g.reacted_by_me} title={`${g.emoji} ${g.count}`} onclick={() => togglePill(msg.message_id, g.emoji)}>{g.emoji} {g.count}</button>
                {/each}
              </div>
            {/if}
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#if mode === 'new'}
      <!-- new-thread recipient picker -->
      <div class="recipient-picker">
        <div class="chips">
          <!-- eslint-disable-next-line -->
          {#each newCompose?.recipient_picker?.chips ?? [] as chip, i}
            <span data-testid={IDS.RECIPIENT_PICKER_CHIP} class="chip">{chipDisplay(chip)}</span>
          {/each}
        </div>
        <input
          data-testid={IDS.RECIPIENT_PICKER_INPUT}
          class="input"
          type="text"
          placeholder={t.conversations.unified.recipient_picker_placeholder}
          value={newCompose?.recipient_picker?.raw_input ?? ''}
          oninput={onRecipientInput}
          onkeydown={onRecipientKeydown}
        />
        <!-- Chrome, like the empty-list line: ui.yaml gives it no id. -->
        {#if bridgeLabels}
          <p class="muted">{t.conversations.unified.recipient_picker_bridges({ bridges: bridgeLabels })}</p>
        {/if}
        <div class="suggestions">
          <!-- eslint-disable-next-line -->
          {#each newCompose?.recipient_picker?.suggestions ?? [] as sug, i}
            <button data-testid={IDS.RECIPIENT_PICKER_SUGGESTION} class="suggestion" onclick={() => run(async () => { await manager?.resolveRecipient(); manager?.acceptCurrentRecipientChip(); })}>{addrDisplay(sug)}</button>
          {/each}
        </div>
        <span data-testid={IDS.RECIPIENT_RESOLVE_STATUS} data-state={newComposeResolve.token} class="resolve-status">
          {newComposeResolve.label ? resolveLocalized(newComposeResolve.label) : ''}
        </span>
        <!-- The class of the room about to be created, stated once a chip is
             committed and before the first message goes out
             (`conversation-rooms.md` § The three classes). Derived in shared
             Rust from the committed chips and the home-nest choice — this
             only paints it. The token rides `data-class` for the same reason
             the thread header's does. -->
        {#if newComposeRoomClass}
          <span
            data-testid={IDS.RECIPIENT_PICKER_CLASS}
            class="room-class"
            data-class={roomClassAttrToken(newComposeRoomClass)}
            >{roomClassLabel(newComposeRoomClass)}</span
          >
        {/if}
        {#if (newCompose?.recipient_picker?.chips?.length ?? 0) >= 2}
          <span data-testid={IDS.GROUP_CONVERSATION_HINT} class="group-hint">{t.conversations.unified.group_conversation_hint}</span>
        {/if}
      </div>
    {/if}

    <!-- compose bar (thread + new-thread) -->
    {#if mode === 'new' || mode === 'thread'}
      {#if activeCompose}
        <div class="compose-bar">
          <!-- Editable reply "To" line — mail only (supports_recipient_selection);
               on FaunaMls the recipients ARE the group membership, so it's hidden.
               Always visible on mail (empty until a reply seeds it); removing a chip
               drops a recipient from THIS reply only. -->
          {#if mode === 'thread' && caps?.supports_recipient_selection}
            <div class="to-line">
              <span class="to-label">{t.conversations.unified.to_line_label}</span>
              <!-- eslint-disable-next-line -->
              {#each activeCompose.reply_recipients ?? [] as r, i}
                <span class="to-chip">
                  <span data-testid={IDS.DM_REPLY_RECIPIENT_CHIP}>{addrDisplay(r)}</span>
                  <button class="chip-x" data-testid={IDS.DM_REPLY_RECIPIENT_REMOVE} title={t.common.remove} onclick={() => removeReplyRecipient(addrDisplay(r))}>×</button>
                </span>
              {/each}
              <input
                data-testid={IDS.DM_REPLY_RECIPIENT_ADD}
                class="to-add"
                type="text"
                placeholder={t.conversations.unified.reply_recipient_add_placeholder}
                value={replyRecipientInput}
                oninput={(e) => (replyRecipientInput = (e.target as HTMLInputElement).value)}
                onkeydown={onReplyRecipientKeydown}
              />
            </div>
          {/if}
          {#if activeCompose.reply_to}
            <div class="reply-preview" data-testid={IDS.DM_REPLY_PREVIEW}>
              {replyPreviewText}
              <button class="link-btn" data-testid={IDS.DM_REPLY_CANCEL} onclick={cancelReply}>{t.common.cancel}</button>
            </div>
          {/if}
          <!-- eslint-disable-next-line -->
          {#each activeCompose.attachments ?? [] as att, i}
            <span class="to-chip">
              <span data-testid={IDS.DM_COMPOSE_ATTACHMENT_CHIP}>{att.filename} {byteSize(att.size_bytes)}</span>
              <button class="chip-x" data-testid={IDS.DM_COMPOSE_ATTACHMENT_REMOVE} title={t.common.remove} onclick={() => removeComposeAttachment(i)}>×</button>
            </span>
          {/each}
          {#if activeCompose.subject_draft != null}
            <input
              data-testid={IDS.SUBJECT_INPUT}
              class="input"
              type="text"
              placeholder={t.conversations.unified.topic_input_placeholder}
              value={activeCompose.subject_draft}
              oninput={onSubject}
            />
          {/if}
          <MarkdownToolbar
            value={activeCompose.body_draft ?? ''}
            onchange={setBody}
            disabled={!markdownEnabled}
            wrap={editorWrap ?? undefined}
            markersShown={composeMarkersShown}
            onToggleMarkers={() => (composeMarkersShown = !composeMarkersShown)}
          />
          <MarkdownEditor
            bind:wrapSelection={editorWrap}
            value={activeCompose.body_draft ?? ''}
            oninput={setBody}
            placeholder={t.conversations.compose.write_message}
            markdownEnabled={markdownEnabled}
            markersShown={composeMarkersShown}
          />
          <div class="compose-actions">
            {#if mode === 'new'}
              <button data-testid={IDS.NEW_CONVERSATION_CANCEL} class="action-btn" onclick={cancelNewConversation}>{t.common.cancel}</button>
            {/if}
            <button data-testid={IDS.TOPIC_TOGGLE_BUTTON} class="action-btn" class:disabled={isGated(caps?.supports_subject)} data-disabled={String(isGated(caps?.supports_subject))} disabled={isGated(caps?.supports_subject)} onclick={() => { if (!isGated(caps?.supports_subject)) toggleTopic(); }}>{t.conversations.unified.topic_toggle_add}</button>
            <label class="action-btn attachment-field" class:disabled={isGated(caps?.supports_attachments)}>
              {t.conversations.unified.attachment_button}{#if activeCompose.attachments?.length} ({activeCompose.attachments.length}){/if}
              <input
                data-testid={IDS.ATTACHMENT_BUTTON}
                class:disabled={isGated(caps?.supports_attachments)}
                data-disabled={String(isGated(caps?.supports_attachments))}
                disabled={isGated(caps?.supports_attachments)}
                type="file"
                multiple
                onchange={(e) => {
                  const input = e.currentTarget;
                  if (isGated(caps?.supports_attachments)) {
                    input.value = '';
                    return;
                  }
                  stageAttachments(input.files).then(() => { input.value = ''; });
                }}
              />
            </label>
            <button data-testid={IDS.DM_SEND_BUTTON} class="action-btn primary" onclick={send}>{t.common.send}</button>
          </div>
        </div>
      {/if}
    {/if}

    <!-- add-participant overlay -->
    {#if addPart}
      <div class="overlay">
        <div class="overlay-card">
          <h3>{t.conversations.unified.thread_add_participant}</h3>
          <div class="chips">
            <!-- eslint-disable-next-line -->
            {#each addPart.picker?.chips ?? [] as chip, i}
              <span data-testid={IDS.RECIPIENT_PICKER_CHIP} class="chip">{addrDisplay(chip)}</span>
            {/each}
          </div>
          <input
            data-testid={IDS.RECIPIENT_PICKER_INPUT}
            class="input"
            type="text"
            placeholder={t.conversations.unified.recipient_picker_placeholder}
            value={addPart.picker?.raw_input ?? ''}
            oninput={onRecipientInput}
            onkeydown={onRecipientKeydown}
          />
          <span data-testid={IDS.RECIPIENT_RESOLVE_STATUS} data-state={addPartResolve.token} class="resolve-status">
            {addPartResolve.label ? resolveLocalized(addPartResolve.label) : ''}
          </span>
          <div class="overlay-actions">
            <button class="action-btn" onclick={cancelAddParticipant}>{t.common.cancel}</button>
            <button data-testid={IDS.ADD_PARTICIPANT_CONFIRM} class="action-btn primary" onclick={confirmAddParticipant}>{t.common.add}</button>
          </div>
        </div>
      </div>
    {/if}

    <!-- room policy editor (`room_settings` sub-page) -->
    {#if roomSettingsOpen && roomDraft && detail}
      <!-- ⚠ This editor must OUTLIVE its own Save: the contract is "closes
           only when all landed", and `apply_room_settings` stops at the first
           refusal. A container that dismisses itself on any response is the
           wrong primitive (linux had to drop `adw::MessageDialog` for exactly
           this); the plain conditional block here closes only where
           `saveRoomSettings` says so. -->
      <div class="overlay">
        <!-- Esc cancels, like the rename overlay (no cancel id —
             `ui/conversations.md` § Element IDs). -->
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <div
          class="overlay-card"
          role="dialog"
          tabindex="-1"
          onkeydown={(e: KeyboardEvent) => {
            if (e.key === 'Escape') cancelRoomSettings();
          }}
        >
          <h3>{t.conversations.unified.thread_room_settings}</h3>

          <label class="room-row">
            <span>{t.conversations.unified.room_join_rule_label}</span>
            <select
              data-testid={IDS.ROOM_JOIN_RULE_SELECT}
              class="input"
              value={roomJoinRuleToken(roomDraftJoinRule)}
              onchange={stageJoinRule}
            >
              {#each roomJoinRuleEditorChoices() as choice}
                <option value={choice.token}>{choice.label}</option>
              {/each}
            </select>
          </label>

          <label class="room-row">
            <span>{t.conversations.unified.room_history_policy_label}</span>
            <select
              data-testid={IDS.ROOM_HISTORY_POLICY_SELECT}
              class="input"
              value={roomHistoryPolicyToken(roomDraftHistoryPolicy)}
              onchange={stageHistoryPolicy}
            >
              {#each roomHistoryPolicyEditorChoices() as choice}
                <option value={choice.token}>{choice.label}</option>
              {/each}
            </select>
          </label>

          <!-- One row per member, indexed exactly like `thread-member-chip[i]`
               so a driver's index means the same person on both. Every state
               below — staged admin, staged hand-over, and whether either
               control may act on this row at all — is READ FROM THE DRAFT
               (`roomSettingsRows`), never re-derived here: the owner's own row
               and a non-Fauna row are ineligible, and the at-most-one-staged
               hand-over rule lives in the draft too. -->
          {#each detail.participant_displays ?? [] as disp, i}
            {@const row = roomSettingsRows[i]}
            <div class="room-row">
              <span>{disp}</span>
              <button
                data-testid={IDS.ROOM_ADMIN_TOGGLE}
                class="action-btn"
                data-checked={row?.admin ? 'true' : 'false'}
                disabled={!detail.capabilities?.can_appoint_admins || !row?.eligible}
                onclick={() => stageAdmin(i)}
              >
                {row?.admin
                  ? t.conversations.unified.room_admin_yes
                  : t.conversations.unified.room_admin_no}
              </button>
              <button
                data-testid={IDS.ROOM_OWNER_TRANSFER_BUTTON}
                class="action-btn"
                data-checked={row?.transfer ? 'true' : 'false'}
                disabled={!detail.capabilities?.can_transfer_ownership || !row?.eligible}
                onclick={() => stageTransfer(i)}
              >
                {row?.transfer
                  ? t.conversations.unified.room_transfer_staged
                  : t.conversations.unified.room_transfer_mark}
              </button>
            </div>
          {/each}

          <div class="overlay-actions">
            <button class="action-btn" onclick={cancelRoomSettings}>{t.common.cancel}</button>
            <button
              data-testid={IDS.ROOM_SETTINGS_SAVE_BUTTON}
              class="action-btn primary"
              onclick={saveRoomSettings}>{t.common.save}</button
            >
          </div>
        </div>
      </div>
    {/if}

    <!-- rename overlay -->
    {#if renaming}
      <div class="overlay">
        <div class="overlay-card">
          <h3>{t.conversations.unified.thread_rename}</h3>
          <input
            data-testid={IDS.THREAD_RENAME_FIELD}
            class="input"
            type="text"
            placeholder={t.conversations.unified.thread_rename_placeholder}
            bind:value={renameValue}
          />
          <div class="overlay-actions">
            <button class="action-btn" onclick={cancelRename}>{t.common.cancel}</button>
            <button data-testid={IDS.THREAD_RENAME_CONFIRM} class="action-btn primary" onclick={confirmRename}>{t.common.save}</button>
          </div>
        </div>
      </div>
    {/if}
  </section>
</div>

<style>
  .conversations { display: flex; height: calc(100vh - 3rem); gap: 0; }
  .list-pane {
    width: 320px; flex-shrink: 0; border-right: 1px solid var(--border);
    display: flex; flex-direction: column; overflow: hidden;
  }
  .list-top { display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem 0.75rem; }
  .list-top h1 { font-size: 1.25rem; margin: 0; flex: 1; }
  .icon-btn {
    border: 1px solid var(--border); border-radius: 6px; background: var(--bg-surface);
    color: var(--text); cursor: pointer; padding: 0.25rem 0.6rem; font-size: 1rem;
  }
  .icon-btn:hover { background: var(--bg-hover); }
  .search {
    margin: 0 0.75rem 0.5rem; padding: 0.4rem 0.6rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg); color: var(--text); font-size: 0.85rem;
  }
  .rows { flex: 1; overflow-y: auto; }
  .row {
    display: flex; align-items: center; gap: 0.5rem; width: 100%; text-align: left;
    padding: 0.6rem 0.75rem; border: none; border-bottom: 1px solid var(--border);
    background: transparent; color: var(--text); cursor: pointer;
  }
  .row:hover { background: var(--bg-hover); }
  .row.selected { background: var(--bg-hover); }
  .row-text { display: flex; flex-direction: column; min-width: 0; flex: 1; }
  .row-label { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .row-snippet { font-size: 0.8rem; color: var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .row-time { font-size: 0.75rem; color: var(--text-muted); flex-shrink: 0; white-space: nowrap; align-self: flex-start; }
  .unread-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); flex-shrink: 0; }
  .protocol-icon { flex-shrink: 0; }

  .detail-pane { flex: 1; display: flex; flex-direction: column; overflow: hidden; position: relative; padding: 0.75rem; }
  .error { color: var(--danger, #e74c3c); min-height: 1rem; font-size: 0.8rem; margin: 0 0 0.5rem; }
  .hint { margin: auto; }
  .thread-header {
    display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap;
    padding-bottom: 0.5rem; border-bottom: 1px solid var(--border); margin-bottom: 0.5rem;
  }
  .thread-title { font-weight: 700; font-size: 1.05rem; }
  .members { display: flex; gap: 0.25rem; flex-wrap: wrap; flex: 1; }
  .chip { background: var(--bg-hover); border-radius: 999px; padding: 0.1rem 0.5rem; font-size: 0.75rem; display: inline-flex; align-items: center; gap: 0.35rem; flex-wrap: wrap; }
  /* Only a membership-change-capable chip is Remove, so only that one points. */
  .chip.removable { cursor: pointer; }
  .chip.greyed { opacity: 0.55; cursor: not-allowed; }
  /* The room's class statement, on the thread header and on the new-thread
     picker alike — a quiet caption, like linux's dim caption label. */
  .room-class { font-size: 0.75rem; color: var(--text-muted); }
  .guardian-state { font-size: 0.75rem; color: var(--text-muted); flex-shrink: 0; }
  /* One row of the room policy editor: the caption or member name takes the
     slack, the controls sit at the end. */
  .room-row { display: flex; align-items: center; gap: 0.5rem; }
  .room-row > span:first-child { flex: 1; }
  /* The post-succession review pair, INSIDE the chip it decides. The mark is a
     full sentence, so a flagged chip wraps rather than truncating the one
     explanation the owner has. */
  .chip-mark { color: var(--text-muted); }
  .chip-keep { border: 1px solid var(--border); border-radius: 6px; background: var(--bg-surface); color: var(--text); cursor: pointer; padding: 0 0.35rem; font-size: 0.75rem; }
  .hdr-btn { border: 1px solid var(--border); border-radius: 6px; background: var(--bg-surface); color: var(--text); cursor: pointer; padding: 0.2rem 0.5rem; font-size: 0.8rem; }
  .messages { flex: 1; overflow-y: auto; display: flex; flex-direction: column; gap: 0.5rem; }
  .subject-divider { text-align: center; font-size: 0.75rem; color: var(--text-muted); border-top: 1px solid var(--border); padding-top: 0.4rem; }
  .bubble { background: var(--bg-surface); border: 1px solid var(--border); border-radius: 8px; padding: 0.5rem 0.6rem; }
  .bubble-head { display: flex; align-items: center; gap: 0.4rem; flex-wrap: wrap; margin-bottom: 0.25rem; }
  .sender { font-family: monospace; font-size: 0.75rem; color: var(--text-muted); }
  .bubble-time { font-size: 0.75rem; color: var(--text-muted); white-space: nowrap; margin-left: auto; }
  .badge { font-size: 0.7rem; padding: 0.05rem 0.3rem; border: 1px solid var(--accent); border-radius: 4px; }
  .reply-btn { margin-left: auto; border: none; background: none; color: var(--accent); cursor: pointer; font-size: 0.75rem; }
  .bubble-body { font-size: 0.875rem; white-space: pre-wrap; }
  /* Reactions & message delete (conversations.md § Reactions & message delete). */
  .message-deleted { font-size: 0.85rem; font-style: italic; color: var(--text-muted); }
  .actions-btn {
    border: none; background: none; color: var(--text-muted); cursor: pointer;
    font-size: 1rem; line-height: 1; padding: 0 0.25rem;
  }
  .actions-btn:hover { color: var(--text); }
  /* Inline ⋯ flyout-equivalent (apple's inline @State pattern): a small bordered
     card under the bubble head with the quick-set row + more + delete. */
  .actions-menu {
    display: flex; flex-wrap: wrap; align-items: center; gap: 0.2rem;
    margin: 0.25rem 0; padding: 0.25rem 0.35rem;
    border: 1px solid var(--border); border-radius: 8px; background: var(--bg-surface);
    width: fit-content;
  }
  .reaction-option, .grid-emoji {
    border: none; background: none; cursor: pointer; font-size: 1.05rem;
    line-height: 1; padding: 0.15rem 0.25rem; border-radius: 6px;
  }
  .reaction-option:hover, .grid-emoji:hover { background: var(--bg-hover); }
  .reaction-more, .delete-option {
    border: 1px solid var(--border); border-radius: 6px; background: var(--bg);
    color: var(--text); cursor: pointer; font-size: 0.78rem; padding: 0.15rem 0.4rem;
  }
  .reaction-more:hover { background: var(--bg-hover); }
  .delete-option { color: var(--danger, #e74c3c); border-color: var(--danger, #e74c3c); }
  .delete-option:hover { background: rgba(231, 76, 60, 0.1); }
  .more-grid {
    display: grid; grid-template-columns: repeat(5, 1fr); gap: 0.2rem;
    margin: 0.25rem 0; padding: 0.35rem; max-width: 14rem;
    border: 1px solid var(--border); border-radius: 8px; background: var(--bg-surface);
  }
  .delete-confirm {
    display: flex; align-items: center; gap: 0.5rem;
    margin: 0.25rem 0; padding: 0.35rem 0.5rem;
    border: 1px solid var(--danger, #e74c3c); border-radius: 8px; background: var(--bg-surface);
    width: fit-content;
  }
  .confirm-title { font-size: 0.8rem; font-weight: 600; }
  .delete-confirm-btn {
    border: 1px solid var(--danger, #e74c3c); border-radius: 6px;
    background: var(--danger, #e74c3c); color: #fff; cursor: pointer;
    font-size: 0.78rem; padding: 0.2rem 0.6rem;
  }
  .reaction-pills { display: flex; flex-wrap: wrap; gap: 0.25rem; margin-top: 0.35rem; }
  .reaction-pill {
    border: 1px solid var(--border); border-radius: 999px; background: var(--bg-surface);
    color: var(--text); cursor: pointer; font-size: 0.75rem; padding: 0.1rem 0.5rem;
  }
  .reaction-pill:hover { background: var(--bg-hover); }
  .reaction-pill.mine { background: var(--accent); color: #fff; border-color: var(--accent); }
  /* In-bubble reply-quote (render-model.md § D2): an accent-bar card above the body
     with the parent author and a snippet clamped to ≤ 2 lines (the user-approved shape). */
  .dm-message-quote {
    display: flex; flex-direction: column; gap: 0.1rem;
    border-left: 3px solid var(--accent); padding: 0.15rem 0.5rem; margin-bottom: 0.3rem;
    background: var(--bg-muted, rgba(127,127,127,0.08)); border-radius: 0 4px 4px 0;
  }
  .quote-author { font-size: 0.72rem; font-weight: 600; color: var(--accent); }
  .quote-snippet {
    font-size: 0.8rem; color: var(--text-muted);
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2;
    overflow: hidden;
  }
  /* Blocked-by-default inbound remote image (no `src`): a dashed placeholder
     showing the alt text, never a fetched/broken image. The reveal button below
     loads it (html-mail.md § Rendering + § Security & privacy). */
  .bubble-body :global(img.blocked-remote-image) {
    display: inline-block; min-width: 6rem; min-height: 1.5rem; max-width: 100%;
    padding: 0.4rem 0.6rem; border: 1px dashed var(--border); border-radius: 6px;
    background: var(--bg); color: var(--text-muted); font-size: 0.78rem; font-style: italic;
  }
  .load-remote-btn { display: inline-block; margin: 0.25rem 0 0; }
  /* Rendered attachments — image painted from the shared `attachmentBytes`
     loader, or a file affordance carrying the filename + size. */
  .dm-attachment { display: block; margin-top: 0.35rem; }
  img.dm-attachment-image {
    max-width: 240px; max-height: 200px; border-radius: 6px;
    border: 1px solid var(--border); object-fit: contain;
  }
  .dm-attachment-file { font-size: 0.82rem; }
  a.dm-attachment-file { color: var(--accent); text-decoration: none; }
  a.dm-attachment-file:hover { text-decoration: underline; }
  span.dm-attachment-file { color: var(--text-muted); }

  .recipient-picker { display: flex; flex-direction: column; gap: 0.4rem; padding-bottom: 0.5rem; }
  .chips { display: flex; gap: 0.25rem; flex-wrap: wrap; }
  .suggestions { display: flex; flex-direction: column; }
  .suggestion { text-align: left; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-surface); padding: 0.3rem 0.5rem; cursor: pointer; }
  .resolve-status { font-size: 0.78rem; color: var(--text-muted); }
  .group-hint { font-size: 0.78rem; color: var(--text-muted); }

  .compose-bar { display: flex; flex-direction: column; gap: 0.4rem; border-top: 1px solid var(--border); padding-top: 0.5rem; }
  .to-line { display: flex; align-items: center; gap: 0.3rem; flex-wrap: wrap; }
  .to-label { font-size: 0.78rem; color: var(--text-muted); font-weight: 600; }
  .to-chip { display: inline-flex; align-items: center; gap: 0.2rem; background: var(--bg-hover); border-radius: 999px; padding: 0.1rem 0.5rem; font-size: 0.75rem; }
  .chip-x { border: none; background: none; color: var(--text-muted); cursor: pointer; font-size: 0.85rem; line-height: 1; padding: 0; }
  .chip-x:hover { color: var(--danger, #e74c3c); }
  .to-add { flex: 1; min-width: 120px; padding: 0.25rem 0.5rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font-size: 0.8rem; }
  .reply-preview { font-size: 0.78rem; color: var(--text-muted); }
  .link-btn { border: none; background: none; color: var(--accent); cursor: pointer; font-size: 0.75rem; }
  .input { padding: 0.45rem 0.6rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); font-size: 0.875rem; width: 100%; }
  .compose-actions { display: flex; gap: 0.5rem; align-items: center; }
  .action-btn { border: 1px solid var(--border); border-radius: 6px; background: var(--bg-surface); color: var(--text); cursor: pointer; padding: 0.35rem 0.7rem; font-size: 0.85rem; }
  .action-btn:hover:not(:disabled) { background: var(--bg-hover); }
  .action-btn:disabled, .action-btn.disabled { opacity: 0.45; cursor: not-allowed; pointer-events: none; }
  .action-btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); margin-left: auto; }
  .attachment-field { position: relative; cursor: pointer; }
  /* Visually hidden, not display:none — stays focusable/reachable and
     Playwright's set_input_files (CDP-based) targets it regardless of
     visibility; the label wraps it so a real click anywhere on the pill
     forwards to the input natively (no JS ref/click() needed). */
  .attachment-field input[type='file'] {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    opacity: 0;
  }

  .overlay { position: absolute; inset: 0; background: rgba(0,0,0,0.35); display: flex; align-items: center; justify-content: center; }
  .overlay-card { background: var(--bg-surface); border: 1px solid var(--border); border-radius: 10px; padding: 1rem; width: 340px; max-width: 90%; display: flex; flex-direction: column; gap: 0.5rem; }
  .overlay-card h3 { margin: 0; font-size: 1rem; }
  .overlay-actions { display: flex; gap: 0.5rem; justify-content: flex-end; }

  .muted { color: var(--text-muted); padding: 0 0.75rem; }

  @media (max-width: 768px) {
    .conversations { flex-direction: column; }
    .list-pane { width: 100%; border-right: none; border-bottom: 1px solid var(--border); }
  }
</style>
