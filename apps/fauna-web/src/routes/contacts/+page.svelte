<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { identity, reconnectTick, knocks as knocksStore, contacts as contactsStore } from '$lib/store';
  import {
    fetchKnocks,
    fetchContacts,
    acceptKnock,
    blockKnock,
    dismissKnock,
    confirmContact,
    sendKnock,
  } from '$lib/api';
  import { parseRecipient } from '$lib/resolve';
  import { goto } from '$app/navigation';
  import type { Knock, Contact } from '$lib/types';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { relativeTime } from '$lib/value-format';
  import { contactStatusLabel, contactMatchesFilter, shortId, wardAskRules } from '$lib/wasm';
  import { carddavListAddressbooks, carddavQueryCardsDecoded, carddavLocateCardByUidHash, onPushEvent, staleSurfacesForPushKind } from '$lib/rpc';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { consumePendingSearchNav } from '$lib/search';
  import { IDS } from '$lib/generated/uiIds';
  import { memberReviewRoster } from '$lib/member-reviews';
  import { familyContactRequest } from '$lib/rpc';
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
    type KnockErrorEffect,
    type KnockSendResult,
  } from '$lib/ward-asks';
  import { wardAsks, refreshWardAsks, rereadAfterAsk } from '$lib/wardAsks.svelte';

  let unsubPush: (() => void) | null = null;
  let unsubReconnect: (() => void) | null = null;
  let loading = $state(true);
  let error = $state('');
  let localKnocks = $state<Knock[]>([]);
  let localContacts = $state<Contact[]>([]);
  let findInput = $state('');
  let findStatus = $state<'idle' | 'searching' | 'found' | 'error'>('idle');
  let findResult = $state<{ actorId: string; handle?: string; domain?: string } | null>(null);
  let findError = $state('');
  let sendingKnock = $state(false);
  // The knock + guardian-ask state for the CURRENT Find User result — sent,
  // refused by the guardian gate, asked (family-safety.md § Child-initiated
  // contact requests → App affordance). Owned by the peer it names: every new
  // lookup starts a fresh one, so a refusal is never carried over to somebody
  // the ward did not name (`$lib/ward-asks`).
  let knockAsk = $state<KnockAskState>(knockStateFor(null));
  let askingGuardian = $state(false);
  let contactsSearch = $state('');
  let copiedActorId = $state('');

  // Address Book segment (CardDAV vCards — slice 4b; a separate store from the
  // social contact graph above). Read-only master-detail over the carddav wasm
  // exports (carddav-server.md § Independent enablement, contacts.md § Layout).
  let segment = $state<'people' | 'addressbook'>('people');
  let addressbooks = $state<any[]>([]);
  let selectedBookId = $state<string | null>(null);
  let cards = $state<any[]>([]);
  let selectedCard = $state<any | null>(null);
  let abLoading = $state(false);
  let abError = $state('');

  async function showAddressBook() {
    segment = 'addressbook';
    const id = $identity;
    if (id && addressbooks.length === 0) await loadAddressbooks(id.secretHex);
  }

  async function loadAddressbooks(secretHex: string) {
    abLoading = true;
    abError = '';
    try {
      const { addressbooks: books } = await carddavListAddressbooks(secretHex);
      addressbooks = books;
      // Auto-open the first book so the card list isn't empty on entry.
      if (books.length > 0 && !selectedBookId) await selectBook(books[0].id);
    } catch (e) {
      abError = e instanceof Error ? e.message : t.common.load_failed;
    } finally {
      abLoading = false;
    }
  }

  async function selectBook(bookId: string) {
    selectedBookId = bookId;
    selectedCard = null;
    const id = $identity;
    if (!id) return;
    abLoading = true;
    abError = '';
    try {
      const { cards: cs } = await carddavQueryCardsDecoded(id.secretHex, bookId);
      // A late reply for a book the user has since left is dropped.
      if (selectedBookId === bookId) cards = cs;
    } catch (e) {
      abError = e instanceof Error ? e.message : t.common.load_failed;
    } finally {
      abLoading = false;
    }
  }

  /** The Address Book re-read a `fauna.addressbook.changed` push asks for
   *  (`StaleSurfaces::address_book`, `transport.md` § Push events) — the books,
   *  and the open book's cards, so a card another contacts app writes appears
   *  on the page the user is looking at. Page-gated by the caller: only while
   *  the Address Book segment shows (a contacts app's first sync is one push per
   *  card). Silent — no loading flash — and it keeps the open book open; the
   *  cards land only if that book is still the open one when they arrive. */
  async function refreshAddressBook(secretHex: string) {
    try {
      const { addressbooks: books } = await carddavListAddressbooks(secretHex);
      addressbooks = books;
      const open = selectedBookId;
      if (!open || !books.some((b: any) => b.id === open)) {
        if (books.length > 0) await selectBook(books[0].id);
        return;
      }
      const { cards: cs } = await carddavQueryCardsDecoded(secretHex, open);
      if (selectedBookId !== open) return;
      cards = cs;
      if (selectedCard) selectedCard = cs.find((c: any) => c.id === selectedCard.id) ?? null;
    } catch (e) {
      abError = e instanceof Error ? e.message : t.common.load_failed;
    }
  }

  /** `SearchNav::Contact` deep-link (`../search/+page.svelte`'s `openResult`,
   *  via `$lib/search`'s pending-nav handoff) — resolves the row's `uid_hash`
   *  to its `card_id` over the wire (`carddavLocateCardByUidHash`), since the
   *  two are different id spaces of the same width and a naive cast would
   *  open nothing (`$lib/search`'s doc comment). Lands on the Address Book
   *  segment with the found book selected and its cards loaded off the SAME
   *  response (no second `carddavQueryCardsDecoded` round trip). A vanished
   *  card (deleted since it was indexed) surfaces the shared "not found"
   *  banner rather than a blank segment. */
  async function openContactByUidHash(secretHex: string, uidHash: string) {
    segment = 'addressbook';
    abLoading = true;
    abError = '';
    try {
      const result = await carddavLocateCardByUidHash(secretHex, uidHash);
      addressbooks = result.addressbooks;
      if (result.addressbook_id && result.card_id) {
        selectedBookId = result.addressbook_id;
        cards = result.cards;
        selectedCard = cards.find((c: any) => c.id === result.card_id) ?? null;
      } else {
        // No book holds that uid_hash any more — the card was deleted between
        // being indexed and being clicked (search.md § Where logic lives ->
        // Result navigation (deep link), the Contact bullet's DROPPED clause;
        // matches tui's Outcome::CardLocated handling).
        selectedCard = null;
        abError = t.contacts.address_book.card_not_found;
      }
    } catch (e) {
      abError = e instanceof Error ? e.message : t.common.load_failed;
    } finally {
      abLoading = false;
    }
  }

  async function copyActorId(actorId: string) {
    await navigator.clipboard.writeText(actorId);
    copiedActorId = actorId;
    setTimeout(() => { if (copiedActorId === actorId) copiedActorId = ''; }, 1500);
  }

  onMount(async () => {
    const id = $identity;
    if (!id) {
      loading = false;
      return;
    }
    // The ward's own outstanding asks, so `contact-request-pending` is honest
    // on an open that never saw the refusal (best-effort, not awaited).
    void refreshWardAsks(id.secretHex);
    await refresh(id.secretHex);

    // A `SearchNav::Contact` deep-link left by the Search page
    // (`$lib/search`'s `consumePendingSearchNav`) — after the ordinary refresh
    // above so the social-contact list is already current when the Address
    // Book segment takes over.
    const pendingNav = consumePendingSearchNav();
    if (pendingNav?.kind === 'contact') {
      await openContactByUidHash(id.secretHex, pendingNav.uidHash);
    }

    // Live refresh — the web twin of linux's central push dispatch
    // (`app.rs`: `PushEvent::Knock` → `fetch_knocks()`). Before this, a contact
    // request arriving while the user sat on this page stayed invisible until
    // they navigated away and back: the page had no poll and no push, so mount
    // was its only fetch. Silent, so the live re-pull doesn't flash the list
    // back to its loading state under the user. Checked through the shared
    // classifier (`transport.md` § Which surfaces a push invalidates) rather
    // than matching `kind` by hand — this also makes
    // `fauna.protocol.resync_required` refresh this page, which the old
    // exact-match check never did.
    unsubPush = onPushEvent((kind) => {
      const stale = staleSurfacesForPushKind(kind);
      const id2 = $identity;
      if (!id2) return;
      if (stale.knocks) void refresh(id2.secretHex, { silent: true });
      if (stale.address_book && segment === 'addressbook') void refreshAddressBook(id2.secretHex);
    });

    // Reconnect backstop: pushes fired while the socket was down are never
    // replayed, so a page that stayed mounted across the gap must re-pull. Linux
    // sweeps knocks + contacts on both `Reconnected` and `ResyncRequired` for
    // exactly this reason. `reconnectTick` is a `writable(0)` — it fires once on
    // subscribe, so skip that seed value (the feed page's idiom).
    let firstTick = true;
    unsubReconnect = reconnectTick.subscribe(() => {
      if (firstTick) { firstTick = false; return; }
      const id3 = $identity;
      if (id3) void refresh(id3.secretHex, { silent: true });
    });
  });

  onDestroy(() => {
    unsubPush?.();
    unsubReconnect?.();
  });

  /** The page's one fetch path (knocks + contacts). `silent` skips the loading
   *  flip for the push / reconnect arms, which re-pull under a page the user is
   *  already reading. */
  async function refresh(secretHex: string, opts: { silent?: boolean } = {}) {
    if (!opts.silent) loading = true;
    error = '';
    try {
      const [k, c] = await Promise.all([
        fetchKnocks(secretHex),
        fetchContacts(secretHex),
      ]);
      localKnocks = k;
      localContacts = c;
      knocksStore.set(k);
      contactsStore.set(c);
    } catch (e) {
      error = e instanceof Error ? e.message : t.common.load_failed;
    } finally {
      loading = false;
    }
  }

  async function handleAccept(peerId: string) {
    const id = $identity;
    if (!id) return;
    try {
      await acceptKnock(id.secretHex, peerId);
      await refresh(id.secretHex);
    } catch (e) {
      error = e instanceof Error ? e.message : 'Accept failed';
    }
  }

  async function handleBlock(peerId: string) {
    const id = $identity;
    if (!id) return;
    try {
      await blockKnock(id.secretHex, peerId);
      await refresh(id.secretHex);
    } catch (e) {
      error = e instanceof Error ? e.message : 'Block failed';
    }
  }

  async function handleDismiss(peerId: string) {
    const id = $identity;
    if (!id) return;
    try {
      await dismissKnock(id.secretHex, peerId);
      await refresh(id.secretHex);
    } catch (e) {
      error = e instanceof Error ? e.message : 'Dismiss failed';
    }
  }

  async function handleConfirm(peerId: string) {
    const id = $identity;
    if (!id) return;
    try {
      await confirmContact(id.secretHex, peerId);
      await refresh(id.secretHex);
    } catch (e) {
      error = e instanceof Error ? e.message : 'Confirm failed';
    }
  }

  async function handleFind() {
    if (!findInput.trim()) return;
    findStatus = 'searching';
    findResult = null;
    findError = '';
    knockAsk = knockStateFor(null);
    try {
      const result = await parseRecipient(findInput);
      findResult = { actorId: result.actorId, handle: result.handle, domain: result.domain };
      knockAsk = knockStateFor(result.actorId);
      findStatus = 'found';
    } catch (e: any) {
      findStatus = 'error';
      findError = e.message || t.common.not_found;
    }
  }

  // Send a knock (contact request) to the looked-up actor. The signed
  // `(ContactRequest, Post)` tuple is composed by shared Rust and carried by
  // `fauna.inbox.send`; the home nest local-delivers, or originates
  // `fauna.federation.inbox.deliver` for a cross-nest peer. Same path linux /
  // windows / android take (api-layers.md § Contacts & Knocks). The reply's
  // `inbox_id` is `null` when the peer's `allow_knock` mode parked it as a
  // pending knock — a successful send either way.
  //
  // A supervised ward whose new contacts need guardian approval gets the nest's
  // TYPED refusal: it stays on `error-message` (the send genuinely did not
  // happen) but is no longer a dead end — `contact-request-guardian-button`
  // paints beside it. Any other failure stays an ordinary failure: an ask
  // painted on a transport error would tell an unsupervised user their account
  // is supervised. The reply folds by the peer it was SENT to, so a reply that
  // outlives its lookup never paints on the next result.
  async function handleAddContact() {
    const id = $identity;
    if (!id || !findResult || knockAsk.knockSent) return;
    const peer = findResult.actorId;
    sendingKnock = true;
    let result: KnockSendResult;
    try {
      await sendKnock(id.secretHex, peer);
      result = KNOCK_SENT;
    } catch (e) {
      result = classifyKnockFailure(e);
    }
    const folded = foldKnockReply(knockAsk, peer, result);
    knockAsk = folded.state;
    showKnockError(folded.error);
    sendingKnock = false;
    if (result.kind === 'sent') await refresh(id.secretHex);
  }

  function showKnockError(effect: KnockErrorEffect) {
    if (effect.kind === 'clear') error = '';
    else if (effect.kind === 'guardian') error = t.contacts.guardian_approval_required;
    else error = refusalText(effect.error) || t.common.error;
  }

  /** `contact-request-guardian-button` — ask the guardian to approve the
   *  refused peer (`fauna.family.contact.request`), then re-read the ward's own
   *  `status.contact_requests` so the pending state is the NEST's, durable
   *  across navigation. The ask's own typed refusals (cap reached, peer blocked,
   *  knob off) are the ward's to read verbatim. */
  async function handleAskGuardian() {
    const id = $identity;
    const peer = knockAsk.peer;
    if (!id || !peer || knockAsk.askSent) return;
    askingGuardian = true;
    try {
      await familyContactRequest(id.secretHex, hexToBytes(peer));
      knockAsk = foldContactAsked(knockAsk, peer);
      error = '';
      await rereadAfterAsk(id.secretHex);
    } catch (e) {
      error = refusalText(e) || t.common.error;
    } finally {
      askingGuardian = false;
    }
  }


  const statusOrder = ['confirmed', 'accepted', 'pending', 'blocked'];

  function contactsByStatus(status: string): Contact[] {
    // Shared roster-filter predicate (`fauna_core::format::contact_matches_filter`):
    // case-insensitive substring over handle/domain/actor-id, empty query matches all
    // — replaces web's prior actor-id-only `.includes` (contacts.md § Where logic lives).
    return localContacts.filter(
      (c) => c.status === status && contactMatchesFilter(contactsSearch, c.handle, c.domain, c.peer_id),
    );
  }
</script>

<div data-testid={IDS.CONTACTS_VIEW}>
<h1 data-testid={IDS.PAGE_HEADING}>{t.common.contacts}</h1>

<MessageBanner bind:error />

<!-- Contacts | Address Book segment toggle (slice 4b) -->
<div class="segment" role="tablist">
  <button
    data-testid={IDS.CONTACTS_SEGMENT_PEOPLE}
    class="seg-btn"
    class:active={segment === 'people'}
    role="tab"
    aria-selected={segment === 'people'}
    onclick={() => (segment = 'people')}
  >{t.common.contacts}</button>
  <button
    data-testid={IDS.CONTACTS_SEGMENT_ADDRESSBOOK}
    class="seg-btn"
    class:active={segment === 'addressbook'}
    role="tab"
    aria-selected={segment === 'addressbook'}
    onclick={showAddressBook}
  >{t.contacts.address_book.title}</button>
</div>

{#if segment === 'people'}
<!-- Find User -->
<section class="section">
  <h2>{t.contacts.find_user.title}</h2>
  <p class="muted">{t.contacts.find_user.description}</p>
  <div class="find-form">
    <input
      data-testid={IDS.CONTACT_ACTOR_ID_FIELD}
      type="text"
      class="input"
      placeholder={t.contacts.find_user.placeholder}
      bind:value={findInput}
      onkeydown={(e) => { if (e.key === 'Enter') handleFind(); }}
    />
    <button data-testid={IDS.CONTACT_ACTOR_ID_LOOKUP} class="btn primary" onclick={handleFind} disabled={findStatus === 'searching' || !findInput.trim()}>
      {findStatus === 'searching' ? t.common.searching : t.contacts.find_user.find}
    </button>
  </div>
  {#if findStatus === 'found' && findResult}
    {@const askRender = contactAskRender(
      wardAskRules,
      wardAsks().contact,
      findResult.actorId,
      knockAsk.peer === findResult.actorId && knockAsk.guardianRefused,
      knockAsk.peer === findResult.actorId && knockAsk.askSent,
    )}
    <div class="find-result">
      <span class="find-label">{t.common.actor_id}:</span>
      <span data-testid={IDS.CONTACT_ACTOR_ID_RESULT} class="mono find-actor-id" title={findResult.actorId}>{findResult.actorId}</span>
      {#if findResult.handle && findResult.domain}
        <span class="find-label">{t.common.handle}:</span>
        <span class="find-handle">{findResult.handle}@{findResult.domain}</span>
      {/if}
      <button
        data-testid={IDS.CONTACTS_ADD_BUTTON}
        class="btn primary small"
        disabled={sendingKnock || knockAsk.knockSent}
        onclick={handleAddContact}
      >{knockAsk.knockSent ? t.contacts.sent : t.contacts.knock}</button>
      <!-- The ward's in-app ask (family-safety.md § Child-initiated contact
           requests → App affordance): the durable pending state first (the
           ward's own status.contact_requests), else the ask — offered only
           after the TYPED guardian refusal of a knock to this very peer. -->
      {#if askRender === 'pending'}
        <span data-testid={IDS.CONTACT_REQUEST_PENDING} class="muted small">{t.contacts.contact_request_pending}</span>
      {:else if askRender === 'ask'}
        <button
          data-testid={IDS.CONTACT_REQUEST_GUARDIAN_BUTTON}
          class="btn small"
          disabled={askingGuardian}
          onclick={handleAskGuardian}
        >{t.contacts.ask_guardian}</button>
      {/if}
    </div>
  {:else if findStatus === 'error'}
    <p class="error small" data-testid={IDS.CONTACT_FIND_ERROR}>{findError}</p>
  {/if}
</section>

<!-- Contacts Search — always visible so tests can interact regardless of load state -->
<section class="section">
  <h2>{t.common.contacts}</h2>
  <input
    data-testid={IDS.CONTACTS_SEARCH_FIELD}
    type="text"
    class="input search-input"
    placeholder={t.common.search}
    bind:value={contactsSearch}
  />
</section>

{#if !$identity}
  <p class="muted">{t.contacts.sign_in_prompt}</p>
{:else if loading}
  <p class="muted">{t.common.loading}</p>
{:else}
  <!-- Message Requests (Knocks) -->
  <section class="section">
    <h2>{t.contacts.message_requests.title}</h2>
    {#if localKnocks.length === 0}
      <p class="muted">{t.contacts.message_requests.none}</p>
    {:else}
      <div class="knock-list">
        {#each localKnocks as knock (knock.id)}
          <div data-testid={IDS.KNOCK_CARD} class="knock-card">
            <div class="knock-info">
              <span data-testid={IDS.KNOCK_SENDER} class="peer-id mono">{shortId(knock.sender)}</span>
              <span class="muted small">{knock.sender_node}</span>
              {#if knock.summary}
                <span class="summary">{knock.summary}</span>
              {/if}
              <!-- knock.created_at is epoch millis (KnockItem) → as-is -->
              <span class="muted small">{relativeTime(knock.created_at)}</span>
            </div>
            <div class="knock-actions">
              <button data-testid={IDS.CONTACTS_ACCEPT_BUTTON} class="btn primary small" onclick={() => handleAccept(knock.sender)}>{t.common.accept}</button>
              <button data-testid={IDS.KNOCK_DISMISS} class="btn small" onclick={() => handleDismiss(knock.sender)}>{t.common.dismiss}</button>
              <button data-testid={IDS.CONTACTS_BLOCK_BUTTON} class="btn danger small" onclick={() => handleBlock(knock.sender)}>{t.common.block}</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <!-- Contacts List -->
  <section class="section">
    {#if localContacts.length === 0}
      <p class="muted">{t.contacts.no_contacts}</p>
    {:else}
      {@const visibleGroups = statusOrder
        .map((status) => ({ status, group: contactsByStatus(status) }))
        .filter(({ group }) => group.length > 0)}
      {#if visibleGroups.length === 0}
        <!-- Roster non-empty but contactsSearch narrowed every status group to
             zero — a distinguishable message from t.contacts.no_contacts above
             (contacts.md § Errors & edge cases: web previously rendered
             nothing here, silently indistinguishable from "still loading"). -->
        <p data-testid={IDS.CONTACTS_NO_MATCHES} class="muted">{t.contacts.no_matching_contacts}</p>
      {:else}
        {#each visibleGroups as { status, group } (status)}
          <div class="status-group">
            <h3 class="status-heading">{resolveLocalized(contactStatusLabel(status))}</h3>
            {#each group as contact (contact.peer_id)}
              <div data-testid={IDS.CONTACT_ROW} class="contact-row">
                <!-- Tap-through to the contact's profile (profile.md
                     § Relationship to Contacts). The button carries the FULL
                     peer hex as its test id (mirrors the linux contact row's
                     widget_name → open_profile(Some(hex))), so the e2e taps it
                     by id; the inner span keeps `contact-name` for the text. -->
                <button
                  type="button"
                  class="contact-open"
                  data-testid={contact.peer_id}
                  title={contact.peer_id}
                  onclick={() => goto(`/app/profile/${contact.peer_id}`)}
                >
                  <span data-testid={IDS.CONTACT_NAME} class="peer-id mono">{shortId(contact.peer_id)}</span>
                </button>
                <button
                  data-testid={IDS.CONTACT_ACTOR_ID_COPY_BTN}
                  class="btn-copy"
                  title={t.common.copy}
                  onclick={() => copyActorId(contact.peer_id)}
                >{copiedActorId === contact.peer_id ? t.common.copied : t.common.copy}</button>
                <span data-testid={IDS.CONTACT_STATUS} class="badge badge-{contact.status}">{resolveLocalized(contactStatusLabel(contact.status))}</span>
                {#if $memberReviewRoster.includes(contact.peer_id)}
                  <!-- The post-succession review badge — the second of
                       succession-aftermath.md § Propagation's three renderings of
                       the one flag (the conversations-page member chip pair is the
                       load-bearing one; the permanent Settings view is the third).
                       Badge only, deliberately with no Keep/Remove pair: the
                       decision belongs where removal already lives (the group
                       member chip), and this row has no eviction affordance to
                       join. -->
                  <span data-testid={IDS.CONTACT_UNATTESTED_MARK} class="badge badge-unattested">{t.contacts.unattested_mark}</span>
                {/if}
                {#if contact.status === 'accepted'}
                  <button data-testid={IDS.CONTACT_CONFIRM} class="btn small" onclick={() => handleConfirm(contact.peer_id)}>{t.common.confirm}</button>
                {/if}
              </div>
            {/each}
          </div>
        {/each}
      {/if}
    {/if}
  </section>
{/if}

{:else}
<!-- Address Book (CardDAV vCards — read-only master-detail, slice 4b) -->
<div class="addressbook">
  <div class="ab-sidebar">
    {#if abLoading && addressbooks.length === 0}
      <p class="muted">{t.common.loading}</p>
    {:else if addressbooks.length === 0}
      <p class="muted">{t.contacts.address_book.no_addressbooks}</p>
    {:else}
      {#each addressbooks as book (book.id)}
        <button
          data-testid={IDS.ADDRESSBOOK_ITEM}
          class="ab-item"
          class:active={selectedBookId === book.id}
          onclick={() => selectBook(book.id)}
        >
          <span class="ab-name">{book.name || t.contacts.address_book.title}</span>
          <span class="ab-count">{book.card_count}</span>
        </button>
      {/each}
    {/if}
  </div>

  <div class="ab-list">
    {#if selectedBookId && cards.length === 0}
      <p class="muted">{t.contacts.address_book.no_cards}</p>
    {:else}
      {#each cards as card (card.id)}
        <button
          data-testid={IDS.VCARD_CARD}
          class="vcard-card"
          class:active={selectedCard?.id === card.id}
          onclick={() => (selectedCard = card)}
        >
          <span data-testid={IDS.VCARD_CARD_FN} class="vcard-fn">{card.formatted_name}</span>
        </button>
      {/each}
    {/if}
  </div>

  <div class="ab-detail">
    {#if !selectedCard}
      <p class="muted">{t.contacts.address_book.select_card}</p>
    {:else}
      <h2 data-testid={IDS.VCARD_DETAIL_FN} class="detail-fn">{selectedCard.formatted_name}</h2>
      {#if selectedCard.title}
        <p class="detail-title">{selectedCard.title}</p>
      {/if}
      {#if selectedCard.org?.length}
        <p data-testid={IDS.VCARD_DETAIL_ORG} class="detail-org">{selectedCard.org.filter((s: string) => s).join(' · ')}</p>
      {/if}
      {#each selectedCard.emails as em}
        <div class="detail-row">
          <span class="detail-label">{t.contacts.address_book.email}</span>
          <span data-testid={IDS.VCARD_DETAIL_EMAIL}>{em.value}</span>
        </div>
      {/each}
      {#each selectedCard.tels as tel}
        <div class="detail-row">
          <span class="detail-label">{t.contacts.address_book.phone}</span>
          <span data-testid={IDS.VCARD_DETAIL_TEL}>{tel.value}</span>
        </div>
      {/each}
      {#each selectedCard.addresses as adr}
        <div class="detail-row">
          <span class="detail-label">{t.contacts.address_book.address}</span>
          <span data-testid={IDS.VCARD_DETAIL_ADR}>{adr.formatted}</span>
        </div>
      {/each}
      {#if selectedCard.note}
        <div class="detail-row">
          <span class="detail-label">{t.contacts.address_book.note}</span>
          <span data-testid={IDS.VCARD_DETAIL_NOTE}>{selectedCard.note}</span>
        </div>
      {/if}
    {/if}
  </div>
</div>
{#if abError}
  <p class="error small" data-testid={IDS.ERROR_MESSAGE}>{abError}</p>
{/if}
{/if}
</div>

<style>
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.75rem; font-size: 1.125rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; }
  .error { color: var(--danger); font-size: 0.875rem; margin-top: 0.25rem; }
  .mono { font-family: monospace; font-size: 0.8rem; }

  .input {
    width: 100%; max-width: 480px; padding: 0.5rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--bg); color: var(--text);
    font-family: monospace; font-size: 0.875rem;
  }
  .find-form { display: flex; gap: 0.5rem; align-items: center; margin-bottom: 0.5rem; }
  .find-result {
    display: flex; flex-wrap: wrap; gap: 0.25rem 0.75rem; align-items: baseline;
    padding: 0.5rem 0.75rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); margin-top: 0.5rem; font-size: 0.875rem;
    max-width: 560px;
  }
  .find-label { color: var(--text-muted); font-size: 0.8rem; }
  .find-actor-id { font-family: monospace; font-size: 0.8rem; word-break: break-all; }
  .find-handle { font-weight: 600; }

  .btn {
    padding: 0.5rem 1rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    font-size: 0.875rem; margin-top: 0.5rem;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.primary:hover { filter: brightness(1.1); }
  .btn.danger { border-color: var(--danger); color: var(--danger); }
  .btn.small { padding: 0.25rem 0.625rem; font-size: 0.8rem; margin-top: 0; }

  .knock-list { display: flex; flex-direction: column; gap: 0.75rem; }
  .knock-card {
    display: flex; align-items: flex-start; justify-content: space-between;
    gap: 1rem; padding: 0.75rem; border: 1px solid var(--border);
    border-radius: 8px; background: var(--bg-surface); flex-wrap: wrap;
  }
  .knock-info { display: flex; flex-direction: column; gap: 0.25rem; }
  .knock-actions { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; }
  .summary { font-size: 0.875rem; color: var(--text); max-width: 480px; }

  .status-group { margin-bottom: 1.25rem; }
  .status-heading {
    font-size: 0.8rem; text-transform: uppercase; letter-spacing: 0.05em;
    color: var(--text-muted); margin-bottom: 0.5rem;
  }
  .contact-row {
    display: flex; align-items: center; gap: 0.75rem; padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .contact-row:last-child { border-bottom: none; }

  .badge {
    font-size: 0.7rem; padding: 0.125rem 0.5rem; border-radius: 999px;
    font-weight: 600; text-transform: uppercase;
  }
  .badge-confirmed { background: color-mix(in srgb, var(--accent) 20%, transparent); color: var(--accent); }
  .badge-accepted { background: color-mix(in srgb, #22c55e 20%, transparent); color: #22c55e; }
  .badge-pending { background: color-mix(in srgb, #f59e0b 20%, transparent); color: #f59e0b; }
  .badge-blocked { background: color-mix(in srgb, var(--danger) 20%, transparent); color: var(--danger); }
  .badge-unattested { background: color-mix(in srgb, #f59e0b 20%, transparent); color: #f59e0b; text-transform: none; }

  .search-input { margin-bottom: 0.75rem; max-width: 360px; }
  .btn-copy {
    padding: 0.125rem 0.5rem; font-size: 0.75rem;
    border: 1px solid var(--border); border-radius: 4px;
    background: var(--bg-surface); color: var(--text-muted); cursor: pointer;
  }
  .btn-copy:hover { background: var(--bg-hover); }
  .contact-open {
    border: none; background: none; padding: 0; margin: 0; cursor: pointer;
    text-align: left; color: var(--accent); font: inherit;
  }
  .contact-open:hover { text-decoration: underline; }

  /* --- Address Book segment (slice 4b) --- */
  .segment {
    display: inline-flex; gap: 0; margin-bottom: 1.25rem;
    border: 1px solid var(--border); border-radius: 8px; overflow: hidden;
  }
  .seg-btn {
    padding: 0.4rem 1rem; border: none; background: var(--bg-surface);
    color: var(--text-muted); cursor: pointer; font-size: 0.875rem;
  }
  .seg-btn + .seg-btn { border-left: 1px solid var(--border); }
  .seg-btn:hover { background: var(--bg-hover); }
  .seg-btn.active { background: var(--accent); color: #fff; }

  .addressbook {
    display: grid; grid-template-columns: minmax(140px, 1fr) minmax(160px, 1.2fr) 2fr;
    gap: 1rem; align-items: start;
  }
  @media (max-width: 640px) {
    .addressbook { grid-template-columns: 1fr; }
  }
  .ab-sidebar, .ab-list { display: flex; flex-direction: column; gap: 0.25rem; }
  .ab-item {
    display: flex; justify-content: space-between; align-items: center; gap: 0.5rem;
    padding: 0.5rem 0.625rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    font-size: 0.875rem; text-align: left;
  }
  .ab-item:hover { background: var(--bg-hover); }
  .ab-item.active { border-color: var(--accent); color: var(--accent); }
  .ab-name { font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ab-count { color: var(--text-muted); font-size: 0.75rem; }

  .vcard-card {
    padding: 0.5rem 0.625rem; border: 1px solid var(--border); border-radius: 6px;
    background: var(--bg-surface); color: var(--text); cursor: pointer;
    text-align: left; font-size: 0.875rem;
  }
  .vcard-card:hover { background: var(--bg-hover); }
  .vcard-card.active { border-color: var(--accent); }
  .vcard-fn { font-weight: 500; }

  .ab-detail {
    padding: 1rem; border: 1px solid var(--border); border-radius: 8px;
    background: var(--bg-surface); min-height: 6rem;
  }
  .detail-fn { margin: 0 0 0.25rem; font-size: 1.25rem; }
  .detail-title { margin: 0 0 0.5rem; color: var(--text-muted); }
  .detail-org { margin: 0 0 0.75rem; color: var(--text-muted); font-size: 0.875rem; }
  .detail-row {
    display: flex; gap: 0.75rem; padding: 0.3rem 0;
    border-bottom: 1px solid var(--border); font-size: 0.9rem;
  }
  .detail-row:last-child { border-bottom: none; }
  .detail-label {
    min-width: 5rem; color: var(--text-muted); font-size: 0.75rem;
    text-transform: uppercase; letter-spacing: 0.03em;
  }
</style>
