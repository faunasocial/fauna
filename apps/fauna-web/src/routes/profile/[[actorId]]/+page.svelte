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
  import { identity } from '$lib/store';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { getConversationsManager, refreshConversations } from '$lib/conversations';
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
    profileGet,
    loadProfileEditBase,
    profileSet,
    sendKnock,
    familyContactRequest,
    type SubscriptionTier,
    type SubscriptionRequest,
    type SubscriptionSubscriber,
  } from '$lib/rpc';
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
  } from '$lib/wasm';
  import { readProvenanceFromBytes } from '$lib/c2pa';
  import { nodeUrl, uploadBlobMultipart } from '$lib/api';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onMount } from 'svelte';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import ProviderSection from '$lib/components/payments/ProviderSection.svelte';
  import ClaimSection from '$lib/components/payments/ClaimSection.svelte';
  import AskingPriceInput from '$lib/components/payments/AskingPriceInput.svelte';
  import { t } from '$lib/i18n/strings';
  import { isSafeNavUrl } from '$lib/safe-url';
  import { bestEffortCopyToClipboard } from '$lib/web-publish';
  import { IDS } from '$lib/generated/uiIds';
  import { hexToBytes } from '$lib/hex';
  import { refusalText } from '$lib/guardian-refusal';
  import {
    classifyKnockFailure,
    contactAskRender,
    foldContactAsked,
    foldKnockReply,
    KNOCK_SENT,
    knockStateFor,
    type KnockAskState,
    type KnockSendResult,
  } from '$lib/ward-asks';
  import { wardAsks, refreshWardAsks, rereadAfterAsk } from '$lib/wardAsks.svelte';
  // subscription-tier-form-asking-price moved into the gated payments id table
  // (dynamic-features.md § A gated plane's user-facing INPUTS excise with it;
  // ui.yaml's gated_features.payments.id_prefixes, added 2026-09-06). The
  // render lives in its own `payments/` component, imported only behind
  // `__FAUNA_PAYMENTS__` below, matching ProviderSection/ClaimSection's own
  // isolation (dynamic-features.md § Platform-family surface excision).

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
  const targetActorId = $derived($page.params.actorId);
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
  // `$lib/components/payments/{ProviderSection,ClaimSection}.svelte` for the web
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
  let isBlocked = $state(false);
  let blockBusy = $state(false);
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
  // identity, profile.md § State & data shape), falling back to handle (SELF) →
  // actor_id when no profile is published yet.
  let publishedDisplayName = $state<string | null>(null);
  const handleLabel = $derived(
    publishedDisplayName ||
      (isSelf ? $identity?.handle || $identity?.actorId || '' : viewedActorId || ''),
  );

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
  async function refreshHeaderName(): Promise<void> {
    const s = secret();
    const actor = viewedActorId;
    const other = !isSelf;
    if (!s || !actor) return;
    try {
      const body = await profileGet(s, actor);
      publishedDisplayName = decodeProfileDisplay(body).display_name;
      if (other && viewedActorId === actor) {
        knockRoute = knockRecipientNestUrl(actor, body, nodeUrl());
      }
    } catch {
      publishedDisplayName = null;
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
    isBlocked = false;
    blockBusy = false;
    knockRoute = null;
    knockAsk = knockStateFor(self ? null : id);
    knockBusy = false;
    askingGuardian = false;
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
  // ("Block"), like linux's `Err(_) => false`.
  async function refreshBlockState(): Promise<void> {
    const s = secret();
    if (!s || !viewedActorId) return;
    try {
      const contacts = await contactsList(s);
      // Shared block predicate (`fauna_core::format::contact_row_blocks_actor` over
      // wasm): a `blocked` row whose `peer_id` matches the viewed actor (case-
      // insensitive hex) → blocked. Same rule the native apps fold over the roster.
      isBlocked = contacts.some((c) =>
        contactRowBlocksActor(c.peer_id, c.status, viewedActorId),
      );
    } catch {
      isBlocked = false;
    }
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
    const wasBlocked = isBlocked;
    blockBusy = true;
    try {
      if (wasBlocked) {
        await knocksUnblock(s, viewedActorId);
      } else {
        await knocksBlock(s, viewedActorId);
      }
      error = '';
      isBlocked = !wasBlocked;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      blockBusy = false;
    }
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
  <header class="user-header">
    <div class="avatar" aria-hidden="true">🙂</div>
    <span class="handle" data-testid={IDS.PROFILE_HANDLE}>{handleLabel}</span>
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
        onclick={toggleBlock} disabled={blockBusy}
      >{blockLabel}</button>
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
      onclick={() => (activeTab = 'posts')}
    >{t.profile.posts}</button>
    <button
      class="tab"
      class:active={activeTab === 'tiers'}
      data-testid={IDS.PROFILE_TIERS_TAB}
      onclick={showTiers}
    >{t.profile.tiers}</button>
  </nav>

  <MessageBanner bind:error />

  <!-- ── Posts tab (content TBD — landmark only) ── -->
  <section hidden={activeTab !== 'posts'}>
    <p class="muted">{t.profile.no_posts}</p>
  </section>

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
            <input
              class="input" type="text" placeholder={t.subscriptions.price_hint}
              data-testid={IDS.SUBSCRIPTION_TIER_FORM_PRICE_HINT} bind:value={formPriceHint}
            />
            {#if __FAUNA_PAYMENTS__}
              <AskingPriceInput
                variant="subscription-tier-form"
                placeholder={t.subscriptions.asking_price}
                value={formAskingPrice}
                onvaluechange={(v) => { formAskingPrice = v; }}
              />
            {/if}
            <input
              class="input" type="text" placeholder={t.subscriptions.payment_url}
              data-testid={IDS.SUBSCRIPTION_TIER_FORM_PAYMENT_URL} bind:value={formPaymentUrl}
            />
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
              <span data-testid={IDS.SUBSCRIPTION_TIER_PRICE}>{tier.price_hint ?? ''}</span>
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
              <span data-testid={IDS.SUBSCRIPTION_OFFER_PRICE}>{offer.price_hint ?? ''}</span>
              <span class="muted small" data-testid={IDS.SUBSCRIPTION_OFFER_DESCRIPTION}>{offer.description ?? ''}</span>
              {#if offer.payment_url}
                <button
                  class="btn" data-testid={IDS.SUBSCRIPTION_OFFER_PAYMENT_LINK}
                  onclick={() => openPayment(offer.payment_url!)}
                >{t.subscriptions.payment_url}</button>
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
