<script lang="ts">
  // Profile page — the canonical per-user *detail* surface (profile.md). Renders
  // the viewer's OWN profile at `/app/profile` (the `profile-tab`) and ANYONE
  // ELSE's at `/app/profile/<actor-id-hex>` (reached by tapping a contact row;
  // contacts/+page.svelte). The optional route param `[[actorId]]` is the page's
  // input; everything branches on `is_self` (profile.md § Layout & flow), exactly
  // like linux `build_profile_view(target: Option<hex>)`:
  //   • SELF  → header `profile-edit-button` + the Tiers-tab author management
  //             (§1 My tiers / §2 Pending requests / §3 Subscribers) + edit form.
  //   • OTHER → header `profile-follow-button` (follow = subscribe to the free
  //             "followers" tier) + the Tiers-tab subscriber-browse offers section
  //             (`subscription-offers-section` — one row per offered paid tier).
  //
  // A dumb renderer over the shared `fauna-client-subscriptions` (rpc.ts → wasm
  // `WsRpcClient.subscriptions*` → `SubscriptionsClient`/`SubscriptionsAuthor`).
  // The author-side mint crypto lives Rust-side; the OTHER browse is thin reads +
  // `subscribe` (priority #2). Lifts linux `apps/fauna-linux/src/views/profile/
  // {mod,tiers,offers}.rs`. Rich identity (display name / avatar / bio) is
  // publish-path-gated (profile.md § Implementation status) — the header renders
  // the published display_name, falling back to handle (SELF) / actor_id (OTHER).
  // UX/IDs: tests/e2e-unified/ui.yaml `profile` page.
  import { identity } from '#lib/store';
  import { accountDisplayHandle } from '#lib/accounts';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import {
    contactOverlays,
    contactOverlaysSettled,
    contactsRevision,
    engineRoleDenied,
    getConversationsManager,
    refreshConversations,
    syncFeedRoomPosts,
  } from '#lib/conversations';
  import {
    savePrivateSection,
    type OverlayEditorFace,
    type OverlayFace,
    type OverlayForm,
  } from '#lib/contact-overlay';
  import {
    knocksBlock,
    knocksUnblock,
    contactsList,
    subscriptionsTiersList,
    subscriptionsCreateTier,
    subscriptionsUpdateTier,
    subscriptionsDeleteTier,
    subscriptionsRequestsList,
    subscriptionsApprove,
    subscriptionsReject,
    subscriptionsSubscribersList,
    subscriptionsRemoveSubscriber,
    subscriptionsOffersList,
    subscriptionsSubscribePublishingEk,
    subscriptionsStatusGet,
    profileHeaderRead,
    loadProfileEditBase,
    profileSet,
    sendKnock,
    familyContactRequest,
    type SubscriptionTier,
    type SubscriptionRequest,
    type SubscriptionSubscriber,
  } from '#lib/rpc';
  import {
    ensureWasm,
    decodeProfileDisplay,
    knockRecipientNestUrl,
    buildEditedProfile,
    buildEditedProfileWithImages,
    processAndSealPublicPost,
    logMessage,
    contactToggleBlockLabel,
    contactRowBlocksActor,
    followToggleLabel,
    offerStatusLabel as wasmOfferStatusLabel,
    parseCount,
    wardAskRules,
    type ProfileLinkInput,
  } from '#lib/wasm';
  import { readProvenanceFromBytes } from '#lib/c2pa';
  import { nodeUrl, uploadBlobMultipart } from '#lib/api';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { onMount, onDestroy, untrack } from 'svelte';
  import FeedPostCard from '#lib/components/FeedPostCard.svelte';
  import FeedReplyDialog from '#lib/components/FeedReplyDialog.svelte';
  import FeedTrainSheet from '#lib/components/FeedTrainSheet.svelte';
  import { blobUrl, createFeedPostHost } from '#lib/feed-post-host.svelte';
  import { getFeedManager } from '#lib/feed';
  import { setPendingSearchNav } from '#lib/search';
  import type { WasmFeedManager } from '../../../../static/fauna_wasm.js';
  import MessageBanner from '#lib/components/MessageBanner.svelte';
  import ReportHost from '#lib/components/ReportHost.svelte';
  import { reportActorTarget, type ReportTarget } from '#lib/wasm';
  import ProviderSection from '#lib/components/payments/ProviderSection.svelte';
  import ClaimSection from '#lib/components/payments/ClaimSection.svelte';
  import TierMoneyFields from '#lib/components/payments/TierMoneyFields.svelte';
  import TierPrice from '#lib/components/payments/TierPrice.svelte';
  import OfferPaymentLink from '#lib/components/payments/OfferPaymentLink.svelte';
  import { t } from '#lib/i18n/strings';
  import { isSafeNavUrl } from '#lib/safe-url';
  import { bestEffortCopyToClipboard } from '#lib/web-publish';
  import { IDS } from '#lib/generated/uiIds';
  import { hexToBytes } from '#lib/hex';
  import { refusalText } from '#lib/guardian-refusal';
  import {
    blockEdgeBlocked,
    blockToggleEnabled,
    foldBlockPress,
    foldBlockRead,
    unknownBlockEdge,
  } from '#lib/block-edge';
  import {
    classifyKnockFailure,
    contactAskRender,
    foldContactAsked,
    foldKnockReply,
    KNOCK_SENT,
    knockStateFor,
    type KnockAskState,
    type KnockSendResult,
  } from '#lib/ward-asks';
  import { wardAsks, refreshWardAsks, rereadAfterAsk } from '#lib/wardAsks.svelte';
  // The price-and-route class — the tier editor's three money fields, the
  // tier list's and offer row's price text, the offer's payment link — is
  // inside the `payments` plane (dynamic-features.md § Platform-family surface
  // excision → *The price-and-route class*). Each render lives in its own
  // `payments/` component, imported only behind `__FAUNA_PAYMENTS__` below,
  // matching ProviderSection/ClaimSection's own isolation.

  type Tab = 'posts' | 'tiers';

  // Following = subscribing to the free, always-present "followers" tier
  // (monetization.md), so it is NOT a per-row offer — it is the header
  // `profile-follow-button`. Excluded from the offers list (mirrors linux
  // `offers::FOLLOWERS_TIER`).
  const FOLLOWERS_TIER = 'followers';

  let activeTab = $state<Tab>('posts');
  let error = $state('');

  // The viewed actor: the route param when present (OTHER), else the viewer's own
  // id (SELF). `is_self` drives edit-vs-follow + which Tiers-tab branch renders.
  const targetActorId = $derived(page.params.actorId);
  const viewedActorId = $derived(targetActorId ?? $identity?.actorId ?? '');
  const isSelf = $derived(!targetActorId || targetActorId === $identity?.actorId);

  // §1 My tiers (SELF)
  let tiers = $state<SubscriptionTier[]>([]);
  // §1's OWN row list excludes per-post pay-to-unlock tiers (monetization.md
  // §128; leg (2) of "sell this post" — an auto-minted unlock tier must never
  // appear in the author's tier-management list). `tiers` itself stays
  // unfiltered: §3/§4/§5 still need to pick a designated tier (view its
  // subscribers / mint a claim against it), same shape as linux's
  // `render_tier_rows` vs. `offers.rs`'s `FOLLOWERS_TIER` filter.
  let manageableTiers = $derived(tiers.filter((tr) => tr.unlocks_post == null));
  let formVisible = $state(false);
  // `null` while creating; the tier name while editing (name is the server key,
  // not editable on update).
  let editing = $state<string | null>(null);
  let formName = $state('');
  let formRank = $state('');
  let formDescription = $state('');
  let formPriceHint = $state('');
  // The machine-comparable price, in sats (monetization.md § The asking
  // price) — independent of formPriceHint above; no parsing ever infers one
  // from the other. Empty on create means unpriced; empty on an edit means
  // "keep the current price" (`tiers.update`'s merge rule — no clear verb).
  let formAskingPrice = $state('');
  let formPaymentUrl = $state('');
  let formAutoApprove = $state(false);

  // §2 Pending requests (SELF)
  let requests = $state<SubscriptionRequest[]>([]);
  let requestBusy = $state(false);

  // §3 Subscribers (SELF)
  let subscribers = $state<SubscriptionSubscriber[]>([]);
  let selectedTier = $state('');

  // §4 payment providers and §5 manual claim codes (SELF; monetization.md
  // § Pillar 3) moved WHOLE — state, reads and markup — into
  // `#lib/components/payments/{ProviderSection,ClaimSection}.svelte` for the web
  // `payments` excision leg: this route's chunk ships in every flavor, so a
  // payments face named here would survive into the store-safe bundle even with
  // the render folded away (`dynamic-features.md` § Platform-family surface
  // excision). All that remains here is the reload trigger below.
  //
  // Bumped wherever `loadSelf()` runs, so the two sections re-read on exactly
  // the occasions they used to: mount, viewed-actor change, the Tiers-tab
  // re-click, and after every §1/§2/§3 mutation.
  let selfReloadTick = $state(0);

  // OTHER — subscriber browse (offers + the viewer's status)
  let offers = $state<SubscriptionTier[]>([]);
  // The viewer's currently-active tier for this creator (`status.get`), or null.
  let viewerStatusTier = $state<string | null>(null);
  // Tiers the viewer just queued (encrypted-mode `Queued`) — a transient overlay
  // on the status-derived label until the next reload (mirrors android's set).
  let pendingTiers = $state<Set<string>>(new Set());
  let offerBusy = $state(false);
  // Optimistic follow flip for the encrypted-mode queued case (status won't yet
  // reflect a pending follow); null ⇒ derive from status.
  let followedOverride = $state<boolean | null>(null);
  const isFollowing = $derived(followedOverride ?? viewerStatusTier === FOLLOWERS_TIER);
  // The toggle wording (`following` vs `follow`) lives in shared Rust
  // (`fauna_core::format::follow_toggle_label` over wasm), so web shares the label
  // instead of its own ternary. The `isFollowing` derivation above stays web-local.
  const followLabel = $derived(resolveLocalized(followToggleLabel(isFollowing)));

  // OTHER — secondary relationship actions (start DM + block; profile.md § Layout
  // & flow). `profile-block-button` is the `contact_status`-driven Block⇄Unblock
  // toggle (lead = linux): the label reads "Unblock" when the viewed actor's edge
  // is `blocked`, else "Block"; a tap toggles over `fauna.knocks.{block,unblock}`
  // and re-derives. Mirrors linux `refresh_block_state`/`toggle_block`.
  // The edge is not-yet-known until the open-time read (or the viewer's own press)
  // sets it, and the toggle is disabled until then — see `#lib/block-edge`.
  let blockEdge = $state(unknownBlockEdge());
  let blockBusy = $state(false);
  const isBlocked = $derived(blockEdgeBlocked(blockEdge));
  const blockEnabled = $derived(blockToggleEnabled(blockEdge, blockBusy));
  // The toggle label decision (`blocked` → "Unblock", else "Block") lives in shared
  // Rust (`fauna_core::format::contact_toggle_block_label` over wasm), so web shares
  // the wording instead of its own ternary. The button *style* stays web-local.
  const blockLabel = $derived(resolveLocalized(contactToggleBlockLabel(isBlocked)));

  // OTHER — `profile-request-contact-button`, the knock (profile.md § Where
  // logic lives → Request contact), and the supervised ward's ask pair beside
  // it (family-safety.md § Child-initiated contact requests → App affordance).
  // `knockRoute` is where the knock goes: the shared
  // `fauna_client_profile::knock_recipient_nest_url` over the profile this open
  // fetched (`null` = this nest). `knockAsk` belongs to the actor it names and
  // is replaced on every open, so a refusal never carries to the next profile.
  let knockRoute = $state<string | null>(null);
  let knockAsk = $state<KnockAskState>(knockStateFor(null));
  let knockBusy = $state(false);
  let askingGuardian = $state(false);
  const contactAsk = $derived(
    contactAskRender(
      wardAskRules,
      wardAsks().contact,
      viewedActorId,
      knockAsk.peer === viewedActorId && knockAsk.guardianRefused,
      knockAsk.peer === viewedActorId && knockAsk.askSent,
    ),
  );

  const heading = $derived(activeTab === 'tiers' ? t.profile.tiers : t.profile.posts);

  // The header identity label prefers the published `display_name` (rich
  // identity, profile.md § State & data shape), falling back to — SELF — the
  // handle on the CURRENT primary (`<handle>@<domain>`, re-derived whenever
  // `$identity` moves, so a primary rename lands without a reload;
  // mail-primary-domain-rename.md § Display-handle interaction) → actor_id
  // when no profile is published yet.
  let publishedDisplayName = $state<string | null>(null);
  // The pictures the header read found named (hex blob hashes), and whether
  // this nest served the profile — so its blob route holds them.
  let headerPictures = $state<{ avatar: string | null; banner: string | null; local: boolean }>({
    avatar: null,
    banner: null,
    local: true,
  });
  // The hashes whose `<img>` has decoded — `data-state="painted"`; anything
  // else (loading, a failed fetch) is `placeholder`, never `error-message`.
  let paintedPictures = $state<Record<string, boolean>>({});
  function pictureSrc(hash: string | null): string | undefined {
    return hash && headerPictures.local ? blobUrl(hash) : undefined;
  }
  // On another's profile both names come from the private overlay's one
  // resolver: the viewer's nickname heads the page and the public name it
  // replaced is the secondary line (`profile-public-name`, absent with no
  // nickname) — profile.md § The private section → *The header shows both
  // names*. Re-read when the overlay moves (`$contactsRevision`).
  const otherNames = $derived.by(() => {
    void $contactsRevision;
    if (isSelf || !wasmReady || !viewedActorId) return null;
    return contactOverlays().peerLabel(publishedDisplayName, null, viewedActorId);
  });
  const handleLabel = $derived.by(() => {
    if (!isSelf) return otherNames?.primary ?? viewedActorId ?? '';
    const selfHandle = wasmReady
      ? accountDisplayHandle($identity?.handle, $identity?.domain)
      : undefined;
    return publishedDisplayName || selfHandle || $identity?.actorId || '';
  });

  // ── the private section (OTHER only — profile.md § The private section) ──
  // The staging is the face's (`WasmOverlayEditor` over shared Rust's
  // `OverlayEditor`); this page holds only the text widgets' values. The
  // editor is rebuilt for each viewed person and whenever the tab's face
  // changes (the manager arrived, or the account switched), and its form is
  // re-read on every overlay move, so an untouched section follows a sibling
  // device's edit.
  let privateEditor = $state<OverlayEditorFace | null>(null);
  // Plain (untracked) twin of `privateEditor`, so the effect below never
  // reads the state it writes.
  let privateEditorKey: { face: OverlayFace; actor: string; editor: OverlayEditorFace } | null =
    null;
  let privateForm = $state<OverlayForm>({ nickname: '', notes: '', labels: [] });
  let labelDraft = $state('');
  // The completion rows under the add field: nothing until something is typed;
  // the matching and the staged-label exclusion are shared Rust (wasm).
  const labelSuggestions = $derived.by(() => {
    void $contactsRevision;
    if (!privateEditor || labelDraft.trim() === '') return [];
    return contactOverlays().labelSuggestions(labelDraft, privateForm.labels);
  });
  let privateSaving = $state(false);

  $effect(() => {
    void $contactsRevision;
    void $engineRoleDenied;
    const id = viewedActorId;
    // Built only once the tab's face is settled (`contactOverlaysSettled`): an
    // editor over the runtime-less face while the manager is still building
    // would refuse a Save this tab is about to be able to make.
    if (isSelf || !wasmReady || !id || !contactOverlaysSettled()) {
      privateEditor = null;
      privateEditorKey = null;
      return;
    }
    const face = contactOverlays();
    if (privateEditorKey?.face !== face || privateEditorKey.actor !== id) {
      privateEditorKey = { face, actor: id, editor: face.editor(id) };
      privateEditor = privateEditorKey.editor;
      labelDraft = '';
    }
    privateForm = privateEditorKey.editor.form();
  });

  function setPrivateNickname(value: string): void {
    privateEditor?.setNickname(value);
    if (privateEditor) privateForm = privateEditor.form();
  }

  function setPrivateNotes(value: string): void {
    privateEditor?.setNotes(value);
    if (privateEditor) privateForm = privateEditor.form();
  }

  function addPrivateLabel(): void {
    if (!privateEditor) return;
    const refusal = privateEditor.addLabel(labelDraft);
    if (refusal) {
      error = resolveLocalized(refusal);
      return;
    }
    labelDraft = '';
    privateForm = privateEditor.form();
  }

  function removePrivateLabel(index: number): void {
    if (!privateEditor) return;
    privateEditor.removeLabel(index);
    privateForm = privateEditor.form();
  }

  async function savePrivate(): Promise<void> {
    const editor = privateEditor;
    if (!editor || privateSaving) return;
    privateSaving = true;
    try {
      const refusal = await savePrivateSection(editor);
      // A refused Save keeps the staged edits on screen; a landed one has
      // already reloaded the projection, so the re-read below (and every name
      // on the page) follows it.
      error = refusal ?? '';
      if (refusal === null) refreshConversations();
      if (privateEditor === editor) privateForm = editor.form();
    } finally {
      privateSaving = false;
    }
  }

  // ── profile edit form (SELF only; text-only v1: display_name / bio / links) ──
  // A read-modify-write: open fetches the current profile (the base, so a
  // re-edit preserves avatar/nests/inbox_mode/…), Save rebuilds + signs it via
  // shared Rust (`buildEditedProfile`) → `fauna.profile.set`. profile.md
  // § Where logic lives → Profile publish/edit.
  let editFormVisible = $state(false);
  let editDisplayName = $state('');
  let editBio = $state('');
  let editLinks = $state<ProfileLinkInput[]>([]);
  // The current stored signed bytes — the read-modify-write base. `null` ⇒
  // first publish (the actor has no profile yet → minimal defaults).
  let editBaseBody = $state<Uint8Array | null>(null);

  // `profile-edit-avatar` / `profile-edit-banner` — a real OS file picker (web
  // has one, unlike tui — profile.md § Element IDs). Picking stages the file's
  // bytes; the upload happens at Save, mirroring the feed composer's
  // `compose-file`. `*Clear` stages a remove tap; a later pick overrides it
  // (picking wins over removing, the same rule the feed composer and tui's
  // form follow).
  let editAvatarFile = $state<File | null>(null);
  let editAvatarData = $state<Uint8Array | null>(null);
  let editAvatarClear = $state(false);
  let editBannerFile = $state<File | null>(null);
  let editBannerData = $state<Uint8Array | null>(null);
  let editBannerClear = $state(false);

  function secret(): string | null {
    return $identity?.secretHex ?? null;
  }

  // Read the viewed actor's published profile and surface its display_name in the
  // header. A not-yet-published actor surfaces `fauna.profile.not_found`, which we
  // treat as "no rich identity" (the header falls back to handle/actor_id) — never
  // an error. Works for both SELF and OTHER (the caller's auth, the viewed id).
  //
  // The knock route rides the same read: the profile the header renders is the
  // one whose home nest a knock from this page goes to. Written only while the
  // page still shows the actor it was read for, so a slow read for a profile
  // the user has since left cannot route the next actor's knock.
  //
  // The same read names the header's stored pictures (`profile-avatar` /
  // `profile-banner`, profile.md § Layout & flow): rendered only while the
  // profile names one, painted from the blob route of the nest that served
  // it. web reads this nest only (no `home` yet — the header relay is web's
  // remote-read trickle-down), so a picture painted here is always this nest's
  // `blobUrl`; a profile relayed from a home renders its pictures empty.
  async function refreshHeaderName(): Promise<void> {
    const s = secret();
    const actor = viewedActorId;
    const other = !isSelf;
    if (!s || !actor) return;
    try {
      const read = await profileHeaderRead(s, actor, null);
      if (viewedActorId !== actor) return;
      const display = decodeProfileDisplay(read.body);
      publishedDisplayName = display.display_name;
      headerPictures = {
        avatar: display.avatar_hash_hex,
        banner: display.banner_hash_hex,
        local: read.relayedFrom === null,
      };
      if (other) {
        knockRoute = knockRecipientNestUrl(actor, read.body, nodeUrl());
      }
    } catch {
      if (viewedActorId !== actor) return;
      publishedDisplayName = null;
      headerPictures = { avatar: null, banner: null, local: true };
    }
  }

  // Open the edit form: fetch the current profile as the read-modify-write base
  // and populate the fields (empty on first publish). Shown async after the
  // fetch (mirrors the native "fetch then reveal" flow). The base loads through
  // the shared read-prove-record, so a linkless successor's first save never
  // waits on the sign-in hop (profile.md § After an identity succession).
  async function openEditForm(): Promise<void> {
    const s = secret();
    const actorId = $identity?.actorId;
    if (!s || !actorId) return;
    error = '';
    try {
      const body = await loadProfileEditBase(s);
      if (!body) throw new Error('not published yet');
      editBaseBody = body;
      const d = decodeProfileDisplay(body);
      editDisplayName = d.display_name ?? '';
      editBio = d.bio ?? '';
      editLinks = d.links.map((l) => ({ label: l.label, uri: l.uri }));
    } catch {
      // Not published yet (or a transient read failure) → first-publish defaults.
      editBaseBody = null;
      editDisplayName = '';
      editBio = '';
      editLinks = [];
    }
    // A fresh open starts every image field at Keep/empty (same as tui and
    // the feed composer) — rendering the *stored* picture is the future
    // `user-header` image ID's job, not this form's.
    editAvatarFile = null;
    editAvatarData = null;
    editAvatarClear = false;
    editBannerFile = null;
    editBannerData = null;
    editBannerClear = false;
    editFormVisible = true;
  }

  function addLinkRow(): void {
    editLinks = [...editLinks, { label: '', uri: '' }];
  }

  function removeLinkRow(index: number): void {
    editLinks = editLinks.filter((_, i) => i !== index);
  }

  function cancelEdit(): void {
    editFormVisible = false;
  }

  // Upload one staged avatar/banner image through the ordinary public-post
  // blob path (`media.md` § Encryption at rest — avatar/banner blobs are "the
  // same shape" as post attachments) and return the nest's hash. The
  // thumbnail goes up FIRST and is best-effort, mirroring feed's own
  // `processAndSealPublicPost` upload order and the native
  // `fauna_client::upload_public_post_blob` (which does the same
  // thumbnail-then-primary sequence internally) — a thumbnail failure must
  // not fail the whole save.
  async function uploadProfileImage(secretHex: string, data: Uint8Array, mime: string): Promise<string> {
    const hasC2pa = mime.startsWith('image/') ? (await readProvenanceFromBytes(data, mime)) !== null : false;
    const { sidecar, bytes, thumbnail } = await processAndSealPublicPost(data, mime, hasC2pa);
    if (thumbnail) {
      try {
        await uploadBlobMultipart(secretHex, thumbnail.sidecar, thumbnail.bytes);
      } catch (e) {
        logMessage('warn', 'fauna_web::profile', `blob upload: thumbnail failed (non-fatal): ${e}`);
      }
    }
    return await uploadBlobMultipart(secretHex, sidecar, bytes);
  }

  async function saveEdit(): Promise<void> {
    const s = secret();
    if (!s) return;
    // Drop blank link rows; trim text fields → null when empty (matches the
    // native `non_empty` handling so a cleared field publishes as absent).
    const links = editLinks
      .map((l) => ({ label: l.label.trim(), uri: l.uri.trim() }))
      .filter((l) => l.label !== '' || l.uri !== '');
    try {
      // A typed pick wins over a pending clear (set above by the picker's
      // onchange, which also un-stages any pending clear); an upload failure
      // fails the whole save rather than silently publishing text-only —
      // the same "never drop a picked file" rule feed's submit_post follows.
      let avatarHash: string | null = null;
      if (editAvatarData) {
        avatarHash = await uploadProfileImage(s, editAvatarData, editAvatarFile?.type || '');
      }
      let bannerHash: string | null = null;
      if (editBannerData) {
        bannerHash = await uploadProfileImage(s, editBannerData, editBannerFile?.type || '');
      }
      const wire = buildEditedProfileWithImages(
        s,
        editBaseBody,
        opt(editDisplayName),
        opt(editBio),
        links,
        editAvatarClear,
        avatarHash,
        editBannerClear,
        bannerHash,
      );
      await profileSet(s, wire);
      error = '';
      editFormVisible = false;
      await refreshHeaderName();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Read §1 tiers + §2 requests, then repopulate the §3 picker (preserving the
  // prior selection by name) and read that tier's roster. SELF author management.
  async function loadSelf(): Promise<void> {
    const s = secret();
    if (!s) return;
    try {
      tiers = await subscriptionsTiersList(s);
      requests = await subscriptionsRequestsList(s);
      selfReloadTick += 1; // §4 + §5 read themselves off this
      const names = tiers.map((tr) => tr.name);
      if (names.length === 0) {
        selectedTier = '';
        subscribers = [];
      } else {
        if (!names.includes(selectedTier)) selectedTier = names[0];
        await loadRoster(selectedTier);
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function loadRoster(tierName: string): Promise<void> {
    const s = secret();
    if (!s || !tierName) {
      subscribers = [];
      return;
    }
    try {
      subscribers = await subscriptionsSubscribersList(s, tierName);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // OTHER — read the creator's offered tiers + the viewer's current status. A
  // failed status read is non-fatal (every row defaults to "not subscribed"),
  // mirroring linux/android. The free "followers" tier is filtered out (header
  // follow button).
  async function loadOffers(): Promise<void> {
    const s = secret();
    if (!s || !viewedActorId) return;
    try {
      try {
        const st = await subscriptionsStatusGet(s, viewedActorId);
        viewerStatusTier = st.tier;
      } catch {
        viewerStatusTier = null;
      }
      offers = (await subscriptionsOffersList(s, viewedActorId)).filter(
        (tr) => tr.name !== FOLLOWERS_TIER,
      );
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // Activating `profile-tiers-tab` re-reads that tab's data — the ruled uniform
  // door (`monetization.md` § Pillar 1 → *The Tiers-tab re-read door*), lifting
  // tui's `Action::ShowTiers` shape. Unconditional on purpose: a re-click while
  // the tab already shows is how a subscriber asks "did the author approve me
  // yet?", and there is no push kind for a subscribe grant to answer it
  // otherwise. Before this the click only set `activeTab`, so on web the answer
  // was "never" — the $effect below is keyed on the viewed actor, not the tab.
  function showTiers(): void {
    activeTab = 'tiers';
    if (!wasmReady || !viewedActorId) return;
    if (isSelf) void loadSelf();
    else void loadOffers();
  }

  // ── The Posts tab (profile.md § Layout & flow → *The Posts tab*) ──
  //
  // The viewed actor's posts, newest first, as the feed paints them. The tab
  // adds no post logic of its own: its rows are an AUTHOR WINDOW of the feed
  // manager — `authorWindow(actorId)`, minted from the timeline's manager on
  // every open of the tab (no client cache — profile.md § Persistence),
  // reading `fauna.feed.author.posts` (feed.md § State & data shape → *The
  // author window*) — painted by the feed's own card over the card host the
  // Feed page uses (`#lib/feed-post-host`), so every verb (like / repost /
  // quote, the ⋯ menu's delete and web verbs, the reply dialog, the tip list,
  // the training sheet) opens HERE and acts on the window. Loading, empty and
  // error come off the window's `FeedSnapshot` exactly as on the feed. The tui
  // twin is `apps/fauna-tui/src/profile/posts.rs`.
  //
  // The wasm window object is deliberately not `$state` (a proxy over a wasm
  // handle is meaningless); `windowSnap` is the reactive read of it.
  let postsWindow: WasmFeedManager | null = null;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let windowSnap = $state<any>(null);
  let windowGen = 0;
  let windowLoadingMore = $state(false);
  let windowRefreshQueued = false;
  let pageDestroyed = false;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const windowPosts = $derived<any[]>(windowSnap?.posts ?? []);
  // The `feed-empty-state` rule through the same shared decision
  // (`FeedSnapshot::empty_state`); the window carries no search, so its one
  // empty arm is "no posts". Absent while the first read is in flight.
  const windowEmpty = $derived(windowSnap?.empty_state != null);
  const windowError = $derived(resolveLocalized(windowSnap?.error));

  /** Re-read the window's snapshot — web has no observer-driven page loop, so
   *  every awaited window call re-reads (the feed page's `refreshFeed`). */
  function refreshWindow(): void {
    if (!postsWindow) return;
    try {
      windowSnap = postsWindow.snapshot();
    } catch (e) {
      logMessage('warn', 'fauna_web::profile', `author window snapshot failed: ${e}`);
    }
  }

  /** The window's observer: one coalesced re-read per burst of notifications,
   *  never re-entering the manager from inside the mutation that notified. */
  function scheduleWindowRefresh(): void {
    if (windowRefreshQueued) return;
    windowRefreshQueued = true;
    queueMicrotask(() => {
      windowRefreshQueued = false;
      refreshWindow();
    });
  }

  const postsHost = createFeedPostHost({
    manager: () => postsWindow,
    posts: () => windowPosts,
    latestPosts: () => windowSnap?.posts ?? [],
    refreshFeed: refreshWindow,
    setError: (message) => { error = message; },
    destroyed: () => pageDestroyed,
  });

  /** The window's in-context trained topic, re-read off its snapshot. */
  const windowTrainTarget = $derived.by<string | null>(() => {
    void windowSnap;
    return postsHost.trainTargetFactor();
  });

  // The cards' render-time resolves (media hash, quoted embed, link preview,
  // unlock price, tips, the rich-render decode), as on the feed.
  $effect(() => {
    for (const p of windowPosts) postsHost.augmentPost(p);
  });

  // A failed read lands on the page's `error-message` while the tab shows.
  $effect(() => {
    const e = windowError;
    if (e && untrack(() => activeTab) === 'posts') error = e;
  });

  /** Activate `profile-posts-tab` — and the page's open, where it is the
   *  default tab: mint the author window afresh and read its first page. */
  async function openPostsTab(): Promise<void> {
    activeTab = 'posts';
    const actor = viewedActorId;
    if (!wasmReady || !actor) return;
    const gen = ++windowGen;
    let timeline: WasmFeedManager;
    try {
      timeline = await getFeedManager();
    } catch (e) {
      if (gen === windowGen) error = t.feed.error_load({ message: String(e) });
      return;
    }
    if (gen !== windowGen || pageDestroyed) return;
    // The room-post seam rides into the window at mint time (the window copies
    // the timeline's installed seams), so install it first: a room post then
    // names its room for a reader seated on it.
    syncFeedRoomPosts();
    const win = timeline.authorWindow(actor);
    win.subscribe({ onChanged: () => { if (postsWindow === win) scheduleWindowRefresh(); } });
    postsWindow = win;
    windowSnap = null;
    postsHost.resetAugmentation();
    refreshWindow();
    try {
      await win.refreshCurrentFeed();
    } catch (e) {
      if (postsWindow === win) error = t.feed.error_load({ message: String(e) });
    } finally {
      if (postsWindow === win) refreshWindow();
    }
  }

  /** The next page — the feed's own load-more gesture: the list's end
   *  scrolling into view (no load-more element; the feed has none). */
  function loadMorePosts(): void {
    const win = postsWindow;
    if (!win || windowLoadingMore || !windowSnap?.has_more || windowSnap?.status === 'Loading') return;
    windowLoadingMore = true;
    win
      .loadMore()
      .catch((e: unknown) => logMessage('warn', 'fauna_web::profile', `author window load more failed: ${e}`))
      .finally(() => {
        windowLoadingMore = false;
        if (postsWindow === win) refreshWindow();
      });
  }

  /** The list end's sentinel: entering the viewport loads the next page. */
  function loadMoreSentinel(node: HTMLElement): { destroy: () => void } {
    const io = new IntersectionObserver(
      (entries) => { if (entries.some((e) => e.isIntersecting)) loadMorePosts(); },
      { rootMargin: '200px' },
    );
    io.observe(node);
    return { destroy: () => io.disconnect() };
  }

  /** A card's activation: the door a search result uses — the feed page's
   *  `post_detail`, the post resolved through the TIMELINE manager's
   *  `resolvePost` (`profile.md` § The Posts tab → *What a row is*). A repost
   *  row opens its original, as on the feed. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function openPostDetail(post: any): void {
    setPendingSearchNav({ kind: 'post', postId: post.reposted_post_id ?? post.post_id });
    void goto('/app/feed');
  }

  /** `feed-post-report-button` on a card here opens the page's report sheet. */
  function reportPost(postId: string): void {
    const target = postsHost.reportTargetFor(postId);
    if (target) reportTarget = target;
  }

  onDestroy(() => {
    pageDestroyed = true;
    postsHost.destroy();
  });

  let wasmReady = $state(false);

  onMount(async () => {
    try {
      await ensureWasm();
      wasmReady = true;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  // Load (re-load on actor change — SvelteKit reuses the component across
  // `/app/profile/<a>` → `/app/profile/<b>` param changes, so an $effect keyed on
  // the viewed actor is the web equivalent of the native "on becoming visible"
  // refresh). Reads the deps synchronously, then dispatches to the right loader.
  $effect(() => {
    const id = viewedActorId;
    const self = isSelf;
    if (!wasmReady || !id) return;
    // Reset the OTHER overlay state when the viewed actor changes.
    copiedActorId = '';
    pendingTiers = new Set();
    followedOverride = null;
    blockEdge = unknownBlockEdge();
    blockBusy = false;
    knockRoute = null;
    knockAsk = knockStateFor(self ? null : id);
    knockBusy = false;
    askingGuardian = false;
    headerPictures = { avatar: null, banner: null, local: true };
    void refreshHeaderName();
    if (self) {
      void loadSelf();
    } else {
      void loadOffers();
      void refreshBlockState();
      // The ward's own outstanding asks, so `contact-request-pending` is honest
      // on an open that never saw the refusal.
      const s = secret();
      if (s) void refreshWardAsks(s);
    }
  });

  // The Posts tab is the default: the page's open — and an open of another
  // actor in the same component — reads the window afresh.
  $effect(() => {
    void viewedActorId;
    if (!wasmReady) return;
    untrack(() => {
      if (activeTab === 'posts') void openPostsTab();
    });
  });

  // ── §1 create/edit form (SELF) ──
  function openCreate(): void {
    editing = null;
    formName = '';
    formRank = '';
    formDescription = '';
    formPriceHint = '';
    formAskingPrice = '';
    formPaymentUrl = '';
    formAutoApprove = false;
    error = '';
    formVisible = true;
  }

  function openEdit(tier: SubscriptionTier): void {
    editing = tier.name;
    formName = tier.name;
    formRank = String(tier.rank);
    formDescription = tier.description ?? '';
    formPriceHint = tier.price_hint ?? '';
    // The reverse of the create/update sats conversion — pre-fill with the
    // tier's current price, or empty for an unpriced tier / a unit this
    // build cannot interpret (fail-closed).
    formAskingPrice = tier.asking_price_sats != null ? String(tier.asking_price_sats) : '';
    formPaymentUrl = tier.payment_url ?? '';
    formAutoApprove = tier.auto_approve;
    error = '';
    formVisible = true;
  }

  function cancelForm(): void {
    editing = null;
    formVisible = false;
  }

  function opt(v: string): string | null {
    const trimmed = v.trim();
    return trimmed === '' ? null : trimmed;
  }

  async function saveForm(): Promise<void> {
    const s = secret();
    const name = formName.trim();
    if (!s || name === '') return;
    // Shared non-negative parse (value-formatting.md § Tier rank) — the prior
    // `parseInt(..) || 0` let a negative through (`-3` is truthy), and the wasm
    // boundary's `u32` turned it into 4294967293, a tier outranking everything.
    const rank = parseCount(formRank) ?? 0;
    const description = opt(formDescription);
    const priceHint = opt(formPriceHint);
    // Same shape as the fields above (and their shared "no clear verb yet"
    // gap — monetization.md § The asking price → Editability): a parsed
    // sats value, `null` for empty OR unparseable. `null` on an edit means
    // "keep the current price", never "clear it".
    const askingPriceRaw = opt(formAskingPrice);
    const askingPrice = askingPriceRaw !== null ? Number(askingPriceRaw) : null;
    const askingPriceSats = askingPrice !== null && Number.isFinite(askingPrice) ? askingPrice : null;
    const paymentUrl = opt(formPaymentUrl);
    try {
      if (editing !== null) {
        await subscriptionsUpdateTier(s, name, rank, description, priceHint, paymentUrl, formAutoApprove, askingPriceSats);
      } else {
        await subscriptionsCreateTier(s, name, rank, description, priceHint, paymentUrl, formAutoApprove, askingPriceSats);
      }
      error = '';
      formVisible = false;
      editing = null;
      await loadSelf();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function deleteTier(name: string): Promise<void> {
    const s = secret();
    if (!s) return;
    try {
      await subscriptionsDeleteTier(s, name);
      await loadSelf();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── §2 approve/reject (SELF) ──
  async function approve(req: SubscriptionRequest): Promise<void> {
    const s = secret();
    if (!s) return;
    requestBusy = true;
    try {
      await subscriptionsApprove(s, req);
      error = '';
      await loadSelf();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      requestBusy = false;
    }
  }

  async function reject(requestId: number): Promise<void> {
    const s = secret();
    if (!s) return;
    try {
      await subscriptionsReject(s, requestId);
      await loadSelf();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── §3 roster (SELF) ──
  async function onSelectTier(e: Event): Promise<void> {
    selectedTier = (e.currentTarget as HTMLSelectElement).value;
    await loadRoster(selectedTier);
  }

  async function removeSubscriber(tierName: string, subscriberIdHex: string): Promise<void> {
    const s = secret();
    if (!s) return;
    try {
      await subscriptionsRemoveSubscriber(s, tierName, subscriberIdHex);
      await loadSelf();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // (§4 provider + §5 claim handlers moved into their components — see the
  // `selfReloadTick` note above.)

  // ── OTHER — subscribe to an offered tier / follow ──
  // Shares the per-tier status derivation (precedence Active>Pending>None) + label
  // map with the native apps via `fauna_core::format::offer_status` over wasm —
  // `viewerStatusTier` is the confirmed held tier (`status.get`), `pendingTiers` the
  // transient post-click set (`status.get` carries no pending discriminant).
  function offerStatusLabel(tier: SubscriptionTier): string {
    return resolveLocalized(
      wasmOfferStatusLabel(tier.name, viewerStatusTier, pendingTiers.has(tier.name)),
    );
  }

  async function subscribeOffer(tier: SubscriptionTier): Promise<void> {
    const s = secret();
    if (!s || !viewedActorId) return;
    offerBusy = true;
    try {
      const r = await subscriptionsSubscribePublishingEk(s, viewedActorId, tier.name);
      error = '';
      if (r.outcome === 'approved') {
        // Granted inline (auto-approve / plaintext) → re-read so the row flips to
        // "Subscribed" off the authoritative status.
        await loadOffers();
      } else {
        // Encrypted-mode pending → overlay "Pending approval" until the next read.
        pendingTiers = new Set([...pendingTiers, tier.name]);
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      offerBusy = false;
    }
  }

  async function follow(): Promise<void> {
    const s = secret();
    if (!s || !viewedActorId) return;
    try {
      await subscriptionsSubscribePublishingEk(s, viewedActorId, FOLLOWERS_TIER);
      error = '';
      followedOverride = true;
      await loadOffers();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // ── OTHER — secondary actions: Start DM + Block ──

  // Start DM — pure client nav glue (no new kind, no persistence): seed the
  // shared Conversations new-thread composer with this actor as a recipient chip,
  // then switch to the Conversations page. The chip carries the real actor_id; its
  // display handle is the actor_id hex (the OTHER header has no cached handle, same
  // fallback as the header label). A synchronous typed-chip seed (no rail probe) —
  // the group bootstraps lazily on the first send. Mirrors linux `profile::start_dm`
  // (`start_new_conversation` + `accept_new_thread_chip(Fauna{..})`); profile.md
  // § Where logic lives → Start DM action.
  async function startDm(): Promise<void> {
    if (!viewedActorId) return;
    try {
      const m = await getConversationsManager();
      m.startNewConversation();
      m.acceptNewThreadFaunaChip(viewedActorId, viewedActorId);
      refreshConversations();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      return;
    }
    await goto('/app/conversations');
  }

  // Read the viewed actor's edge from `fauna.contacts.list` and set the toggle
  // label: a `blocked` row for this peer → "Unblock", else "Block" (mirrors linux
  // `refresh_block_state`). A failed read is non-fatal — defaults to not-blocked
  // ("Block"), like linux's `Err(_) => false` — and still enables the toggle.
  // The read folds through `foldBlockRead`: it is dropped once the viewer's own
  // press authored the edge, and when the viewed actor changed while it was in
  // flight (the component is reused across `/app/profile/<a>` → `<b>`).
  async function refreshBlockState(): Promise<void> {
    const s = secret();
    const actor = viewedActorId;
    if (!s || !actor) return;
    let blocked: boolean;
    try {
      const contacts = await contactsList(s);
      // Shared block predicate (`fauna_core::format::contact_row_blocks_actor` over
      // wasm): a `blocked` row whose `peer_id` matches the viewed actor (case-
      // insensitive hex) → blocked. Same rule the native apps fold over the roster.
      blocked = contacts.some((c) =>
        contactRowBlocksActor(c.peer_id, c.status, actor),
      );
    } catch {
      blocked = false;
    }
    if (actor !== viewedActorId) return;
    blockEdge = foldBlockRead(blockEdge, blocked);
  }

  // Block ⇄ unblock toggle (profile.md § User actions; contacts.md § Where logic
  // lives → Unblock). When not blocked, `fauna.knocks.block` upserts the edge to
  // `blocked`; when blocked, `fauna.knocks.unblock` clears it (guarded
  // clear-the-edge, `ContactStatus` → `None`). On success flip `isBlocked` so the
  // label re-derives; a failed call leaves the state for a retry. Mirrors linux
  // `toggle_block` (the shared `fauna_client_contacts::knocks_{block,unblock}`
  // over wasm; the profile is one caller of the contact-edge lifecycle).
  async function toggleBlock(): Promise<void> {
    const s = secret();
    if (!s || !viewedActorId) return;
    if (!blockEnabled) return;
    const wasBlocked = isBlocked;
    const actor = viewedActorId;
    blockBusy = true;
    try {
      if (wasBlocked) {
        await knocksUnblock(s, viewedActorId);
      } else {
        await knocksBlock(s, viewedActorId);
      }
      error = '';
      if (actor === viewedActorId) blockEdge = foldBlockPress(!wasBlocked);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      blockBusy = false;
    }
  }

  // `profile-report-button` — report this account to the nest admins
  // (moderation.md § User-initiated reporting → App surface), beside block on
  // an OTHER profile. Opens the shared report sheet; a report that also blocked
  // the account flips the block label here too.
  let reportTarget = $state<ReportTarget | null>(null);
  function openReport(): void {
    if (!viewedActorId || isSelf) return;
    error = '';
    reportTarget = reportActorTarget(viewedActorId);
  }

  // `profile-request-contact-button` — the knock, over the contacts page's own
  // `sendKnock` (the profile is a caller of the contact-edge lifecycle, not a
  // second implementation), routed to the viewed profile's home nest. The reply
  // carries the peer it was SENT to and folds against the page as it is now:
  // a "Sent" or a guardian refusal for an actor the user has since left must
  // not paint on the one they are viewing. The TYPED guardian refusal stays on
  // `error-message` and reveals the ask; any other failure is just a failure.
  async function requestContact(): Promise<void> {
    const s = secret();
    const peer = viewedActorId;
    if (!s || !peer || isSelf || knockAsk.knockSent) return;
    knockBusy = true;
    let result: KnockSendResult;
    try {
      await sendKnock(s, peer, knockRoute);
      result = KNOCK_SENT;
    } catch (e) {
      result = classifyKnockFailure(e);
    }
    const folded = foldKnockReply(knockAsk, peer, result);
    knockAsk = folded.state;
    if (folded.error.kind === 'clear') error = '';
    else if (folded.error.kind === 'guardian') error = t.contacts.guardian_approval_required;
    else error = refusalText(folded.error.error) || t.common.error;
    if (viewedActorId === peer) knockBusy = false;
  }

  // `contact-request-guardian-button` — the ward's ask for the viewed actor
  // (`fauna.family.contact.request`), then the durable re-read. The ack flips
  // the just-asked flag only on that actor's own open.
  async function askGuardian(): Promise<void> {
    const s = secret();
    const peer = knockAsk.peer;
    if (!s || !peer || knockAsk.askSent) return;
    askingGuardian = true;
    try {
      await familyContactRequest(s, hexToBytes(peer));
      knockAsk = foldContactAsked(knockAsk, peer);
      error = '';
      await rereadAfterAsk(s);
    } catch (e) {
      error = refusalText(e) || t.common.error;
    } finally {
      askingGuardian = false;
    }
  }

  function openPayment(url: string): void {
    if (typeof window === 'undefined') return;
    // The payment link is nest-supplied; refuse to open a non-https scheme
    // (security.md § Transport trust). `noopener`
    // is already set below; this adds the scheme allowlist a legitimate payment
    // processor URL always satisfies.
    if (!isSafeNavUrl(url)) {
      error = t.subscriptions.unsafe_payment_url;
      return;
    }
    window.open(url, '_blank', 'noopener');
  }

  // The id the copy button last put on the clipboard, published as its `copied`
  // attribute — written from the same string, never re-derived (the web-settings
  // copy-link buttons' contract). Cleared whenever the viewed actor changes.
  let copiedActorId = $state('');

  async function copyActorId(): Promise<void> {
    const id = viewedActorId;
    if (!id) return;
    await bestEffortCopyToClipboard(id);
    if (id === viewedActorId) copiedActorId = id;
  }
</script>

<div class="profile" data-testid={IDS.PROFILE_VIEW}>
  <!-- ── user-header (rich identity is publish-path-gated; render the handle) ── -->
  {#if headerPictures.banner}
    {@const hash = headerPictures.banner}
    <img
      class="banner"
      data-testid={IDS.PROFILE_BANNER}
      data-state={paintedPictures[hash] ? 'painted' : 'placeholder'}
      src={pictureSrc(hash)}
      alt=""
      onload={() => (paintedPictures[hash] = true)}
      onerror={() => (paintedPictures[hash] = false)}
    />
  {/if}
  <header class="user-header">
    {#if headerPictures.avatar}
      {@const hash = headerPictures.avatar}
      <img
        class="avatar"
        data-testid={IDS.PROFILE_AVATAR}
        data-state={paintedPictures[hash] ? 'painted' : 'placeholder'}
        src={pictureSrc(hash)}
        alt=""
        onload={() => (paintedPictures[hash] = true)}
        onerror={() => (paintedPictures[hash] = false)}
      />
    {:else}
      <div class="avatar" aria-hidden="true">🙂</div>
    {/if}
    <span class="handle" data-testid={IDS.PROFILE_HANDLE}>{handleLabel}</span>
    {#if otherNames?.public}
      <span class="muted" data-testid={IDS.PROFILE_PUBLIC_NAME}>{otherNames.public}</span>
    {/if}
    <button
      class="btn"
      data-testid={IDS.PROFILE_ACTOR_ID_COPY_BTN}
      data-copied={copiedActorId || undefined}
      onclick={copyActorId}
    >
      {t.profile.copy_id}
    </button>
    {#if isSelf}
      <button class="btn" data-testid={IDS.PROFILE_EDIT_BUTTON} onclick={openEditForm}>
        {t.profile.edit}
      </button>
    {:else}
      <button class="btn primary" data-testid={IDS.PROFILE_FOLLOW_BUTTON} onclick={follow}>
        {followLabel}
      </button>
    {/if}
  </header>

  <!-- ── OTHER — secondary relationship actions (start DM + block). profile.md
       § Layout & flow: rendered on another's profile only. The block button is
       the `contact_status`-driven Block⇄Unblock toggle; `profile-request-contact-button`
       is the knock, routed to the viewed profile's home nest; the supervised
       ward's ask pair (`contact-request-pending` read from the durable
       status.contact_requests first, else `contact-request-guardian-button`
       only after the TYPED refusal) is the contacts page's, on this page too.
       ── -->
  {#if !isSelf}
    <div class="secondary-actions">
      <button class="btn" data-testid={IDS.PROFILE_START_DM_BUTTON} onclick={startDm}>
        {t.profile.start_dm}
      </button>
      <button
        class="btn danger" data-testid={IDS.PROFILE_BLOCK_BUTTON}
        onclick={toggleBlock} disabled={!blockEnabled}
      >{blockLabel}</button>
      <button
        class="btn danger" data-testid={IDS.PROFILE_REPORT_BUTTON}
        onclick={openReport}
      >{t.profile.report}</button>
      <button
        class="btn" data-testid={IDS.PROFILE_REQUEST_CONTACT_BUTTON}
        onclick={requestContact} disabled={knockBusy || knockAsk.knockSent}
      >{knockAsk.knockSent ? t.profile.request_contact_sent : t.profile.request_contact}</button>
      {#if contactAsk === 'pending'}
        <span class="muted" data-testid={IDS.CONTACT_REQUEST_PENDING}>{t.contacts.contact_request_pending}</span>
      {:else if contactAsk === 'ask'}
        <button
          class="btn" data-testid={IDS.CONTACT_REQUEST_GUARDIAN_BUTTON}
          onclick={askGuardian} disabled={askingGuardian}
        >{t.contacts.ask_guardian}</button>
      {/if}
    </div>
  {/if}

  <!-- ── the private section (OTHER only): your own nickname, notes and labels
       on this person, staged and committed by one Save; nobody else ever sees
       them (profile.md § The private section). Refusals land on the page's
       `error-message`, the staged edits kept. ── -->
  {#if !isSelf && privateEditor}
    <section class="form private-section" data-testid={IDS.PROFILE_PRIVATE_SECTION}>
      <p class="muted small">{t.profile.private_title}</p>
      <label class="field">
        <span>{t.profile.private_nickname}</span>
        <input
          class="input" type="text"
          data-testid={IDS.PROFILE_NICKNAME_FIELD}
          value={privateForm.nickname}
          oninput={(e) => setPrivateNickname((e.currentTarget as HTMLInputElement).value)}
        />
      </label>
      <label class="field">
        <span>{t.profile.private_notes}</span>
        <textarea
          class="input" rows="3"
          data-testid={IDS.PROFILE_NOTES_FIELD}
          value={privateForm.notes}
          oninput={(e) => setPrivateNotes((e.currentTarget as HTMLTextAreaElement).value)}
        ></textarea>
      </label>
      <span>{t.profile.private_labels}</span>
      <div class="list" data-testid={IDS.PROFILE_LABEL_LIST}>
        {#each privateForm.labels as label, i (label)}
          <div class="row">
            <span class="badge" data-testid={IDS.PROFILE_LABEL_CHIP} data-index={i}>{label}</span>
            <button
              class="btn small" data-testid={IDS.PROFILE_LABEL_REMOVE_BUTTON} data-index={i}
              onclick={() => removePrivateLabel(i)}
            >{t.profile.private_label_remove}</button>
          </div>
        {/each}
      </div>
      <div class="row">
        <input
          class="input grow" type="text"
          data-testid={IDS.PROFILE_LABEL_FIELD}
          bind:value={labelDraft}
          onkeydown={(e) => { if (e.key === 'Enter') addPrivateLabel(); }}
        />
        <button class="btn" data-testid={IDS.PROFILE_LABEL_ADD_BUTTON} onclick={addPrivateLabel}>
          {t.profile.private_label_add}
        </button>
      </div>
      {#each labelSuggestions as suggestion, i (suggestion)}
        <button
          class="btn small" data-testid={IDS.PROFILE_LABEL_SUGGESTION} data-index={i}
          onclick={() => { labelDraft = suggestion; addPrivateLabel(); }}
        >{suggestion}</button>
      {/each}
      <div class="form-buttons">
        <button
          class="btn primary" data-testid={IDS.PROFILE_PRIVATE_SAVE_BUTTON}
          onclick={savePrivate} disabled={privateSaving}
        >{t.profile.private_save}</button>
      </div>
    </section>
  {/if}

  <!-- ── profile edit form (SELF, text-only v1) — shown async after the current
       profile is fetched as the read-modify-write base ── -->
  {#if isSelf && editFormVisible}
    <div class="form" data-testid={IDS.PROFILE_EDIT_FORM}>
      <input
        class="input" type="text" placeholder={t.profile.edit_display_name}
        data-testid={IDS.PROFILE_EDIT_DISPLAY_NAME} bind:value={editDisplayName}
      />
      <textarea
        class="input" rows="3" placeholder={t.profile.edit_bio}
        data-testid={IDS.PROFILE_EDIT_BIO} bind:value={editBio}
      ></textarea>

      <div class="list" data-testid={IDS.PROFILE_EDIT_LINK_LIST}>
        {#each editLinks as link, i (i)}
          <div class="row">
            <input
              class="input grow" type="text" placeholder={t.profile.edit_link_label}
              data-testid={IDS.PROFILE_EDIT_LINK_LABEL} data-index={i} bind:value={link.label}
            />
            <input
              class="input grow" type="text" placeholder={t.profile.edit_link_url}
              data-testid={IDS.PROFILE_EDIT_LINK_URL} data-index={i} bind:value={link.uri}
            />
            <button
              class="btn danger" data-testid={IDS.PROFILE_EDIT_LINK_REMOVE_BUTTON}
              data-index={i} onclick={() => removeLinkRow(i)}
            >{t.profile.edit_remove_link}</button>
          </div>
        {/each}
      </div>
      <button class="btn" data-testid={IDS.PROFILE_EDIT_LINK_ADD_BUTTON} onclick={addLinkRow}>
        {t.profile.edit_add_link}
      </button>

      <!-- avatar / banner (real OS file pickers on web; staged, uploaded on
           Save — profile.md § Element IDs, § Where logic lives → Field
           ownership). Mirrors the feed composer's compose-file shape. -->
      <div class="compose-file">
        <input
          class="input" type="file" accept="image/*"
          data-testid={IDS.PROFILE_EDIT_AVATAR}
          onchange={async (e) => {
            const file = (e.currentTarget as HTMLInputElement).files?.[0] ?? null;
            if (file) {
              const buf = await file.arrayBuffer();
              editAvatarFile = file;
              editAvatarData = new Uint8Array(buf);
              editAvatarClear = false;
            } else {
              editAvatarFile = null;
              editAvatarData = null;
            }
          }}
        />
        {#if editAvatarFile}
          <span class="file-name">{editAvatarFile.name}</span>
        {/if}
        <button
          class="btn danger" data-testid={IDS.PROFILE_EDIT_AVATAR_REMOVE_BUTTON}
          onclick={() => { editAvatarClear = true; editAvatarFile = null; editAvatarData = null; }}
        >{t.profile.edit_remove_avatar}</button>
      </div>
      <div class="compose-file">
        <input
          class="input" type="file" accept="image/*"
          data-testid={IDS.PROFILE_EDIT_BANNER}
          onchange={async (e) => {
            const file = (e.currentTarget as HTMLInputElement).files?.[0] ?? null;
            if (file) {
              const buf = await file.arrayBuffer();
              editBannerFile = file;
              editBannerData = new Uint8Array(buf);
              editBannerClear = false;
            } else {
              editBannerFile = null;
              editBannerData = null;
            }
          }}
        />
        {#if editBannerFile}
          <span class="file-name">{editBannerFile.name}</span>
        {/if}
        <button
          class="btn danger" data-testid={IDS.PROFILE_EDIT_BANNER_REMOVE_BUTTON}
          onclick={() => { editBannerClear = true; editBannerFile = null; editBannerData = null; }}
        >{t.profile.edit_remove_banner}</button>
      </div>

      <div class="form-buttons">
        <button class="btn" data-testid={IDS.PROFILE_EDIT_CANCEL_BUTTON} onclick={cancelEdit}>
          {t.profile.edit_cancel}
        </button>
        <button class="btn primary" data-testid={IDS.PROFILE_EDIT_SAVE_BUTTON} onclick={saveEdit}>
          {t.profile.edit_save}
        </button>
      </div>
    </div>
  {/if}

  <h1 class="page-heading" data-testid={IDS.PAGE_HEADING}>{heading}</h1>

  <!-- ── tab strip ── -->
  <nav class="tabs">
    <button
      class="tab"
      class:active={activeTab === 'posts'}
      data-testid={IDS.PROFILE_POSTS_TAB}
      onclick={openPostsTab}
    >{t.profile.posts}</button>
    <button
      class="tab"
      class:active={activeTab === 'tiers'}
      data-testid={IDS.PROFILE_TIERS_TAB}
      onclick={showTiers}
    >{t.profile.tiers}</button>
  </nav>

  <MessageBanner bind:error />
  <ReportHost
    bind:target={reportTarget}
    onerror={(m) => (error = m)}
    onblocked={(actor) => { if (actor === viewedActorId) blockEdge = foldBlockPress(true); }}
  />

  <!-- ── Posts tab — the viewed actor's posts as the feed's own cards, over the
       author window (profile.md § Layout & flow → *The Posts tab*). ── -->
  {#if activeTab === 'posts'}
    <section class="posts-section" data-testid={IDS.PROFILE_POSTS_SECTION}>
      {#if windowEmpty}
        <p class="muted" data-testid={IDS.PROFILE_POSTS_EMPTY}>{t.profile.no_posts}</p>
      {/if}
      {#each windowPosts as post (post.post_id)}
        <FeedPostCard
          {post}
          host={postsHost}
          bridgeRoster={windowSnap?.bridge_roster ?? []}
          trainTarget={windowTrainTarget}
          onreport={reportPost}
          onselect={() => openPostDetail(post)}
        />
      {/each}
      {#if windowSnap?.has_more}
        {#key windowPosts.length}
          <div class="posts-more" use:loadMoreSentinel></div>
        {/key}
      {/if}
      {#if windowLoadingMore}
        <p class="muted">{t.common.loading}</p>
      {/if}
    </section>
  {/if}
  <!-- The card's reply dialog and training sheet open here, in place, over the
       window — the same two the Feed page paints over the timeline. -->
  <FeedReplyDialog host={postsHost} />
  <FeedTrainSheet host={postsHost} />

  <!-- ── Tiers tab — SELF author management vs OTHER subscriber browse ── -->
  <div hidden={activeTab !== 'tiers'}>
    {#if isSelf}
      <!-- §1 My tiers -->
      <section class="section" data-testid={IDS.SUBSCRIPTION_TIERS_SECTION}>
        <h2>{t.subscriptions.my_tiers}</h2>
        <button class="btn primary" data-testid={IDS.SUBSCRIPTION_TIER_CREATE_BUTTON} onclick={openCreate}>
          {t.subscriptions.create_tier}
        </button>

        {#if formVisible}
          <div class="form" data-testid={IDS.SUBSCRIPTION_TIER_FORM}>
            <input
              class="input" type="text" placeholder={t.subscriptions.tier_name}
              data-testid={IDS.SUBSCRIPTION_TIER_FORM_NAME}
              bind:value={formName} disabled={editing !== null}
            />
            <input
              class="input" type="text" inputmode="numeric" placeholder={t.subscriptions.rank}
              data-testid={IDS.SUBSCRIPTION_TIER_FORM_RANK} bind:value={formRank}
            />
            <input
              class="input" type="text" placeholder={t.subscriptions.description}
              data-testid={IDS.SUBSCRIPTION_TIER_FORM_DESCRIPTION} bind:value={formDescription}
            />
            {#if __FAUNA_PAYMENTS__}
              <TierMoneyFields
                bind:priceHint={formPriceHint}
                bind:askingPrice={formAskingPrice}
                bind:paymentUrl={formPaymentUrl}
              />
            {/if}
            <label class="switch-row">
              <span>{t.subscriptions.auto_approve}</span>
              <input
                type="checkbox" data-testid={IDS.SUBSCRIPTION_TIER_FORM_AUTO_APPROVE}
                bind:checked={formAutoApprove}
              />
            </label>
            <div class="form-buttons">
              <button class="btn" data-testid={IDS.SUBSCRIPTION_TIER_FORM_CANCEL} onclick={cancelForm}>
                {t.subscriptions.cancel}
              </button>
              <button class="btn primary" data-testid={IDS.SUBSCRIPTION_TIER_FORM_SAVE} onclick={saveForm}>
                {t.subscriptions.save}
              </button>
            </div>
          </div>
        {/if}

        <div class="list">
          {#each manageableTiers as tier (tier.name)}
            <div class="row" data-testid={IDS.SUBSCRIPTION_TIER_ROW}>
              <span class="grow" data-testid={IDS.SUBSCRIPTION_TIER_NAME}>{tier.name}</span>
              <span data-testid={IDS.SUBSCRIPTION_TIER_RANK}>{tier.rank}</span>
              {#if __FAUNA_PAYMENTS__}
                <TierPrice variant="tier" priceHint={tier.price_hint} />
              {/if}
              <button class="btn" data-testid={IDS.SUBSCRIPTION_TIER_EDIT_BUTTON} onclick={() => openEdit(tier)}>
                {t.subscriptions.edit}
              </button>
              <button
                class="btn danger" data-testid={IDS.SUBSCRIPTION_TIER_DELETE_BUTTON}
                onclick={() => deleteTier(tier.name)}
              >{t.subscriptions.delete}</button>
            </div>
          {/each}
          {#if manageableTiers.length === 0}
            <p class="muted">{t.subscriptions.no_tiers}</p>
          {/if}
        </div>
      </section>

      <!-- §2 Pending requests -->
      <section class="section" data-testid={IDS.SUBSCRIPTION_REQUESTS_SECTION}>
        <h2>{t.subscriptions.pending_requests}</h2>
        <p class="muted small" data-testid={IDS.SUBSCRIPTION_REQUEST_BUSY} hidden={!requestBusy}>
          {t.subscriptions.approving}
        </p>
        <div class="list">
          {#each requests as req (req.request_id)}
            <div class="row" data-testid={IDS.SUBSCRIPTION_REQUEST_ROW}>
              <span class="grow mono" data-testid={IDS.SUBSCRIPTION_REQUEST_SUBSCRIBER}>{req.subscriber_id}</span>
              <span data-testid={IDS.SUBSCRIPTION_REQUEST_TIER}>{req.tier_name}</span>
              <span class="badge" data-testid={IDS.SUBSCRIPTION_REQUEST_KIND}>{req.kind}</span>
              <!-- The payment engine entitled this request (verified webhook or
                   redeemed claim code) — `drain_auto_approvals` approves it
                   without creator judgment. monetization.md § Pillar 3. -->
              {#if req.payment_entitled}
                <span class="badge" data-testid={IDS.SUBSCRIPTION_REQUEST_PAID_BADGE}>
                  {t.subscriptions.paid}
                </span>
              {/if}
              <button
                class="btn primary" data-testid={IDS.SUBSCRIPTION_REQUEST_APPROVE_BUTTON}
                onclick={() => approve(req)} disabled={requestBusy}
              >{t.subscriptions.approve}</button>
              <button
                class="btn" data-testid={IDS.SUBSCRIPTION_REQUEST_REJECT_BUTTON}
                onclick={() => reject(req.request_id)}
              >{t.subscriptions.reject}</button>
            </div>
          {/each}
          {#if requests.length === 0}
            <p class="muted">{t.subscriptions.no_requests}</p>
          {/if}
        </div>
      </section>

      <!-- §3 Subscribers roster -->
      <section class="section" data-testid={IDS.SUBSCRIPTION_SUBSCRIBERS_SECTION}>
        <h2>{t.subscriptions.subscribers}</h2>
        <div class="select-row">
          <label for="subs-tier-select">{t.subscriptions.tier_select_label}</label>
          <select
            id="subs-tier-select" class="input"
            data-testid={IDS.SUBSCRIPTION_SUBSCRIBERS_TIER_SELECT}
            value={selectedTier} onchange={onSelectTier}
          >
            {#each tiers as tier (tier.name)}
              <option value={tier.name}>{tier.name}</option>
            {/each}
          </select>
        </div>
        <div class="list">
          {#each subscribers as sub (sub.subscriber_id)}
            <div class="row" data-testid={IDS.SUBSCRIPTION_SUBSCRIBER_ROW}>
              <span class="grow mono" data-testid={IDS.SUBSCRIPTION_SUBSCRIBER_HANDLE}>{sub.subscriber_id}</span>
              <button
                class="btn danger" data-testid={IDS.SUBSCRIPTION_SUBSCRIBER_REMOVE_BUTTON}
                onclick={() => removeSubscriber(selectedTier, sub.subscriber_id)}
              >{t.subscriptions.remove}</button>
            </div>
          {/each}
          {#if subscribers.length === 0}
            <p class="muted">{t.subscriptions.no_subscribers}</p>
          {/if}
        </div>
      </section>

      <!-- §4 payment providers + §5 manual claim codes (monetization.md
           § Pillar 3), behind the web family's `payments` compile condition.
           Both sections own their reads; `selfReloadTick` is what still ties
           them to this page's refresh occasions. The vite define folds this to
           `false` in the store-safe flavor and the two imports go with it, so
           neither the chunks nor any `subscription-provider-*` /
           `subscription-claim-*` id reaches that bundle
           (`dynamic-features.md` § Platform-family surface excision). -->
      {#if __FAUNA_PAYMENTS__}
        <ProviderSection
          secretHex={$identity?.secretHex ?? null}
          {tiers}
          actorIdHex={$identity?.actorId ?? ''}
          reloadTick={selfReloadTick}
          onerror={(m) => (error = m)}
        />
        <ClaimSection
          secretHex={$identity?.secretHex ?? null}
          {tiers}
          reloadTick={selfReloadTick}
          onerror={(m) => (error = m)}
        />
      {/if}
    {:else}
      <!-- OTHER — subscriber browse: the creator's offered paid tiers. The free
           "followers" tier is the header follow button, so it is excluded here. -->
      <section class="section" data-testid={IDS.SUBSCRIPTION_OFFERS_SECTION}>
        <h2>{t.subscriptions.offers}</h2>
        <div class="list" data-testid={IDS.SUBSCRIPTION_OFFER_LIST}>
          {#each offers as offer (offer.name)}
            <div class="row" data-testid={IDS.SUBSCRIPTION_OFFER_ROW}>
              <span class="grow" data-testid={IDS.SUBSCRIPTION_OFFER_NAME}>{offer.name}</span>
              {#if __FAUNA_PAYMENTS__}
                <TierPrice variant="offer" priceHint={offer.price_hint} />
              {/if}
              <span class="muted small" data-testid={IDS.SUBSCRIPTION_OFFER_DESCRIPTION}>{offer.description ?? ''}</span>
              {#if __FAUNA_PAYMENTS__ && offer.payment_url}
                <OfferPaymentLink onopen={() => openPayment(offer.payment_url!)} />
              {/if}
              <span data-testid={IDS.SUBSCRIPTION_OFFER_STATUS}>{offerStatusLabel(offer)}</span>
              <button
                class="btn primary" data-testid={IDS.SUBSCRIPTION_OFFER_SUBSCRIBE_BUTTON}
                onclick={() => subscribeOffer(offer)} disabled={offerBusy}
              >{t.subscriptions.subscribe}</button>
            </div>
          {/each}
          {#if offers.length === 0}
            <p class="muted">{t.subscriptions.no_offers}</p>
          {/if}
        </div>
      </section>
    {/if}
  </div>
</div>

<style>
  .profile { padding: 1rem; max-width: 900px; }
  .user-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.5rem;
  }
  .avatar {
    width: 48px; height: 48px;
    display: flex; align-items: center; justify-content: center;
    font-size: 1.75rem;
    border-radius: 50%;
    background: var(--bg-surface, #161b22);
    border: 1px solid var(--border, #30363d);
  }
  img.avatar { object-fit: cover; padding: 0; }
  .banner {
    display: block;
    width: 100%;
    height: 120px;
    object-fit: cover;
    border-radius: 6px;
    margin-bottom: 0.5rem;
    background: var(--bg-surface, #161b22);
  }
  .handle { font-weight: 600; font-size: 1.1rem; flex: 1; }
  .secondary-actions { display: flex; gap: 0.5rem; margin-bottom: 0.5rem; }
  .page-heading { font-size: 1.5rem; margin: 0.5rem 0; }
  .tabs { display: flex; gap: 0.5rem; margin-bottom: 1rem; }
  .tab {
    padding: 0.4rem 0.9rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .posts-more { height: 1px; }
  .tab.active { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
  .section {
    margin: 1.25rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .section h2 { font-size: 1rem; margin: 0 0 0.75rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  /* id cells carry the full hex actor id as text (uniform with linux's
     `to_hex()`); truncate visually only, never in the text content. */
  .mono {
    font-family: monospace;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grow { flex: 1; }
  .badge {
    font-size: 0.75rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-hover, #21262d);
    color: var(--text-muted, #8b949e);
  }
  .list { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0.75rem 0;
    padding: 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .form-buttons { display: flex; justify-content: flex-end; gap: 0.5rem; }
  .compose-file { display: flex; align-items: center; gap: 0.5rem; }
  .file-name { font-size: 0.8rem; color: var(--text-muted, #8b949e); }
  .switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
  }
  .select-row { display: flex; align-items: center; gap: 0.5rem; }
  .input {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }
  .btn {
    padding: 0.35rem 0.7rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .btn:hover { background: var(--bg-hover, #21262d); }
  .btn:disabled { opacity: 0.6; cursor: default; }
  .btn.primary { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
  .btn.danger { border-color: var(--danger, #f85149); color: var(--danger, #f85149); }
</style>
