package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContactAskRender
import com.fauna.app.core.HexUtil
import com.fauna.app.core.KnockAskState
import com.fauna.app.core.KnockErrorEffect
import com.fauna.app.core.KnockSend
import com.fauna.app.core.classifyKnock
import com.fauna.app.core.contactAskRender
import com.fauna.app.core.foldContactAskFailed
import com.fauna.app.core.foldContactAsked
import com.fauna.app.core.foldKnockReply
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.app.data.db.*
import com.fauna.ffi.FfiAddressbookRow
import com.fauna.ffi.FfiCardRow
import com.fauna.ffi.actorIdFromSecret
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import javax.inject.Inject

/** The Contacts page's two segments (contacts.md § Layout & flow): the social
 *  contact graph vs. the read-only CardDAV Address Book (slice 4b). */
enum class ContactsSegment { PEOPLE, ADDRESS_BOOK }

@HiltViewModel
class ContactsVM @Inject constructor(
    savedStateHandle: SavedStateHandle,
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val contactDao: ContactDao,
    private val knockDao: KnockDao,
    private val resolveService: ResolveService,
    @ApplicationContext private val context: Context
) : ViewModel() {

    val contacts: Flow<List<Contact>> = contactDao.getAll()
    val knocks: Flow<List<Knock>> = knockDao.getAll()
    val findResult = MutableStateFlow<ResolveService.ResolvedRecipient?>(null)
    val findError = MutableStateFlow<String?>(null)
    val isFinding = MutableStateFlow(false)
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    /**
     * The Find User result's knock + the supervised ward's guardian-ask pair
     * (`contacts-add-button`, `contact-request-guardian-button` /
     * `contact-request-pending` — family-safety.md § Child-initiated contact
     * requests → *App affordance*; tui's `contacts.rs` `knock_sent` /
     * `guardian_refused` / `contact_ask_sent`). Reset by every lookup to the
     * peer it found, so a refusal and an ask belong to the peer they were made
     * for; replies fold through [foldKnockReply] keyed on the peer they were
     * SENT to (rule (g)).
     */
    val knockAsk = MutableStateFlow(KnockAskState.forPeer(null))

    /** The ward's durable own asks ([com.fauna.app.core.WardAsks]) — collected
     *  by the screen so `contact-request-pending` repaints when a status read
     *  lands (rule (c)). */
    val wardContactRequests = api.wardAsks.contactRequests

    /** What the knock / ask put on `error-message` — the guardian refusal stays
     *  a real error there (rule (b)); `null` clears it. */
    val knockError = MutableStateFlow<String?>(null)

    /** The pair's render for [state]'s peer — durable pending first (rule (c)). */
    fun contactAskRenderFor(state: KnockAskState): ContactAskRender? {
        val peer = state.peer ?: return null
        return contactAskRender(api.wardAsks.contactAskPending(peer), state)
    }

    /** The open post-succession member-review roster (succession-aftermath.md
     *  § Propagation → *MLS groups*, item 3a) — the
     *  badge half of the review pair, second reading of the same shared
     *  projection the conversations screen's chip pair reads
     *  ([ConversationsVM.memberReviewRoster]). Refreshed alongside [refresh]
     *  (mount + reconnect + knock push): this app drives no aftermath pump of
     *  its own yet, so this page's own ordinary refresh cadence stands in for
     *  "behind the aftermath's raise" the same way linux's contacts page
     *  does. Best-effort — a roster read failure must never blank an
     *  already-rendered mark or disrupt the contact list refresh beside it. */
    val memberReviewRoster = MutableStateFlow<List<com.fauna.ffi.FfiMemberReview>>(emptyList())

    private fun actorIdHex(): String? =
        secureStorage.secretHex?.let {
            try { HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it))) }
            catch (_: Exception) { null }
        }

    init {
        // Re-fetch knocks + contacts on each WS reconnect (transport.md
        // § Push events), matching the linux WsEvent::Reconnected sweep.
        viewModelScope.launch {
            api.reconnectTick.collect { refresh() }
        }
        // Re-fetch on every inbound fauna.knock push too, so a knock appears on a
        // MOUNTED contacts screen with no navigation — previously only the
        // reconnect sweep recovered it (transport.md § Push events; windows
        // NestRpcClient.StartKnockPump is the reference shape).
        viewModelScope.launch {
            api.knockTick.collect { refresh() }
        }
    }

    fun refresh() {
        viewModelScope.launch {
            isLoading.value = true
            try {
                val actorId = actorIdHex() ?: return@launch

                // Fetch knocks — upsert first, then delete stale entries
                val remoteKnocks = api.fetchKnocks(actorId)
                val remoteKnockIds = remoteKnocks.map { it.id.toInt() }.toSet()
                remoteKnocks.forEach { knockDao.upsert(it.toKnockEntity()) }
                knockDao.getAll().first()
                    .filter { it.id !in remoteKnockIds }
                    .forEach { knockDao.delete(it.id) }

                // Fetch contacts — upsert first, then delete stale entries.
                // fauna.contacts.list is now enriched with handle/domain nest-side
                // for local peers (contacts.md § State & data shape), so prefer those;
                // fall back to any locally-cached values for federated peers the nest
                // can't enrich (it holds no cached federated Profile → handle/domain null).
                val remoteContacts = api.fetchContacts(actorId)
                val remotePeerIds = remoteContacts.map { it.peerId }.toSet()
                val existingMap = contactDao.getAll().first().associateBy { it.peerId }
                remoteContacts.forEach { c ->
                    val existing = existingMap[c.peerId]
                    contactDao.upsert(c.toContactEntity(
                        handle = c.handle ?: existing?.handle,
                        domain = c.domain ?: existing?.domain,
                        nodeUrl = existing?.nodeUrl,
                    ))
                }
                contactDao.getAll().first()
                    .filter { it.peerId !in remotePeerIds }
                    .forEach { contactDao.deleteByPeerId(it.peerId) }

                // Opportunistically resolve handles for contacts STILL missing them after
                // enrichment (federated peers — the nest can't supply handle/domain).
                for (c in remoteContacts) {
                    if ((c.handle ?: existingMap[c.peerId]?.handle) != null) continue
                    try {
                        val resolved = resolveService.resolve(c.peerId)
                        contactDao.upsert(c.toContactEntity(
                            handle = resolved.handle, domain = resolved.domain,
                            nodeUrl = resolved.nodeUrl,
                        ))
                    } catch (_: Exception) {
                        // Resolution failed — leave handle null
                    }
                }
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            isLoading.value = false

            // Independent of the contacts fetch above: a roster-read failure
            // must not blank an already-rendered mark, so it keeps the last
            // value on failure rather than clearing (§ Propagation's ruling —
            // a transport failure must not hide a flagged person).
            try {
                memberReviewRoster.value = api.memberReviewList()
            } catch (_: Exception) {
                // keep the last known roster
            }
        }
    }

    fun findUser(input: String) {
        viewModelScope.launch {
            isFinding.value = true
            findResult.value = null
            findError.value = null
            // A new lookup starts a new pair: a refusal (or a "Sent") for the
            // last result must not carry over to somebody else.
            knockAsk.value = KnockAskState.forPeer(null)
            try {
                val found = resolveService.resolve(input)
                knockAsk.value = KnockAskState.forPeer(found.actorId)
                findResult.value = found
            } catch (e: Exception) {
                findError.value = e.message ?: "Not found"
            }
            isFinding.value = false
        }
    }

    fun acceptKnock(peerId: String) {
        viewModelScope.launch {
            try {
                val actorId = actorIdHex() ?: return@launch
                api.acceptKnock(actorId, peerId)
                refresh()
            } catch (e: Exception) { errorMessage.value = e.message }
        }
    }

    fun blockKnock(peerId: String) {
        viewModelScope.launch {
            try {
                val actorId = actorIdHex() ?: return@launch
                api.blockKnock(actorId, peerId)
                refresh()
            } catch (e: Exception) { errorMessage.value = e.message }
        }
    }

    fun dismissKnock(peerId: String) {
        viewModelScope.launch {
            try {
                val actorId = actorIdHex() ?: return@launch
                api.dismissKnock(actorId, peerId)
                refresh()
            } catch (e: Exception) { errorMessage.value = e.message }
        }
    }

    fun confirmContact(peerId: String) {
        viewModelScope.launch {
            try {
                val actorId = actorIdHex() ?: return@launch
                api.confirmContact(actorId, peerId)
                refresh()
            } catch (e: Exception) { errorMessage.value = e.message }
        }
    }

    /**
     * `contacts-add-button` — knock [peerId] on this nest (a bare id / same-nest
     * lookup names no foreign nest, so no route). The button reads "Sent" only
     * once the nest accepted it; the TYPED guardian refusal re-enables it,
     * reveals the ask, and stays on `error-message` (rules (a), (b)).
     */
    fun addContact(peerId: String) {
        val before = knockAsk.value
        if (before.peer == peerId && (before.knockSent || before.knockInFlight)) return
        val actorId = actorIdHex() ?: return
        if (before.peer == peerId) knockAsk.value = before.copy(knockInFlight = true)
        viewModelScope.launch {
            val result = classifyKnock { api.sendKnock(actorId, peerId) }
            val (next, effect) = foldKnockReply(knockAsk.value, peerId, result)
            knockAsk.value = next
            knockError.value = knockErrorText(effect)
            if (result == KnockSend.Sent) refresh()
        }
    }

    /**
     * `contact-request-guardian-button` — ask the guardian about [peerId]
     * (`fauna.family.contact.request`). A re-ask while one is pending is a
     * quiet nest-side no-op anyway, but not issuing it keeps the affordance
     * honest about its own state.
     */
    fun askGuardian(peerId: String) {
        val before = knockAsk.value
        if (before.peer != peerId || before.askInFlight) return
        if (contactAskRenderFor(before) == ContactAskRender.PENDING) return
        knockAsk.value = before.copy(askInFlight = true)
        viewModelScope.launch {
            try {
                api.familyContactRequest(peerId)
                knockAsk.value = foldContactAsked(knockAsk.value, peerId)
                // The ask landed: the refusal is no longer a dead end.
                knockError.value = null
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                knockAsk.value = foldContactAskFailed(knockAsk.value, peerId)
                // The ask's own typed refusals (cap reached, peer blocked, knob
                // off) are the ward's to read verbatim.
                knockError.value = e.message ?: e.toString()
            }
        }
    }

    private fun knockErrorText(effect: KnockErrorEffect): String? = when (effect) {
        KnockErrorEffect.Clear -> null
        KnockErrorEffect.Guardian -> context.getString(R.string.contacts_guardian_approval_required)
        is KnockErrorEffect.Failed -> effect.message
    }

    // ── Address Book segment (CardDAV vCards — slice 4b, read-only) ──────────────
    //
    // A separate store from the social contact graph above (contacts.md § Layout &
    // flow). Read-only master-detail over the shared carddav seam: the book picker
    // fires `listAddressbooks`, selecting a book fires `queryCards`, and a card tap
    // opens its already-decoded detail (no round-trip). Mirrors the web/linux
    // reference; the msek gate degrades to empty lists inside the seam.

    val segment = MutableStateFlow(ContactsSegment.PEOPLE)
    val addressbooks = MutableStateFlow<List<FfiAddressbookRow>>(emptyList())
    val selectedBookId = MutableStateFlow<String?>(null)
    val cards = MutableStateFlow<List<FfiCardRow>>(emptyList())
    val selectedCard = MutableStateFlow<FfiCardRow?>(null)
    val abLoading = MutableStateFlow(false)
    val abError = MutableStateFlow<String?>(null)

    init {
        // Search deep link (`SearchNav.Contact` — search.md § Where logic
        // lives → *Result navigation (deep link)*; `apps/fauna-tui/src/
        // search.rs` `Action::OpenCardByUid` is the lead-app reference),
        // reached via the `contacts/card/{openUidHash}` route
        // (`FaunaNavHost.kt`) — mirrors [ProfileVM]'s `actorId` arg.
        savedStateHandle.get<String>("openUidHash")?.let { openLocatedCard(it) }
        // Live refresh on `fauna.addressbook.changed` (transport.md § Push
        // events): a card another contacts app writes appears in the open book
        // with no navigation. Page-gated on the Address Book segment — the
        // People half paints nothing this reads, and entering the segment
        // re-reads it anyway (`StaleSurfaces::address_book`).
        viewModelScope.launch {
            api.addressBookChangedTick.collect {
                if (segment.value == ContactsSegment.ADDRESS_BOOK) refreshAddressBook()
            }
        }
    }

    /**
     * Re-read the books and the open book's cards, silently (no loading flash),
     * keeping the open book open — the first book only when the open one is
     * gone. The cards land only if that book is still the open one when they
     * arrive; a late reply for a book the user has left is dropped. tui
     * reference: `contacts.rs::address_book_resync_op`.
     */
    private fun refreshAddressBook() {
        viewModelScope.launch {
            try {
                val books = api.listAddressbooks()
                addressbooks.value = books
                val open = selectedBookId.value
                if (open == null || books.none { it.id == open }) {
                    books.firstOrNull()?.let { selectBook(it.id) }
                    return@launch
                }
                val fresh = api.queryCards(open)
                if (selectedBookId.value != open) return@launch
                cards.value = fresh
                selectedCard.value = selectedCard.value?.let { sel -> fresh.firstOrNull { it.id == sel.id } }
            } catch (e: Exception) {
                abError.value = e.message
            }
        }
    }

    /**
     * Resolve [uidHashHex] to the Address Book's `card_id` over the shared
     * `CardDavClient::locate_card_by_uid_hash` (never a client-side cast —
     * `uid_hash`/`card_id` are different id spaces of the same hex width, so
     * matching one for the other would compile, run, and open nothing) and
     * land straight on the card detail, the same state [selectCard] would
     * leave behind after a manual book→card tap.
     */
    private fun openLocatedCard(uidHashHex: String) {
        segment.value = ContactsSegment.ADDRESS_BOOK
        viewModelScope.launch {
            abLoading.value = true
            abError.value = null
            try {
                val located = api.locateCardByUidHash(uidHashHex)
                addressbooks.value = located.books
                val found = located.found
                if (found != null) {
                    selectedBookId.value = found.addressbookId
                    cards.value = found.cards
                    selectedCard.value = found.cards.firstOrNull { it.id == found.cardId }
                } else {
                    // found == null: the card was deleted since it was indexed —
                    // the DROPPED outcome (search.md § Where logic lives ->
                    // Result navigation (deep link), the Contact bullet's
                    // DROPPED clause), arriving one click late. The books
                    // still loaded, so the segment isn't left empty; say so on
                    // error-message too, or a silently-unchanged page reads as
                    // a dead row (matches tui's Outcome::CardLocated handling).
                    abError.value = context.getString(R.string.contacts_address_book_card_not_found)
                }
            } catch (e: Exception) {
                abError.value = e.message
            }
            abLoading.value = false
        }
    }

    fun showPeople() {
        segment.value = ContactsSegment.PEOPLE
    }

    /** Switch to the Address Book segment, loading the books on first entry. */
    fun showAddressBook() {
        segment.value = ContactsSegment.ADDRESS_BOOK
        if (addressbooks.value.isEmpty()) loadAddressbooks()
    }

    private fun loadAddressbooks() {
        viewModelScope.launch {
            abLoading.value = true
            abError.value = null
            try {
                val books = api.listAddressbooks()
                addressbooks.value = books
                // Auto-open the first book so the card list isn't empty on entry
                // (web parity — loadAddressbooks); a later explicit tap re-fetches
                // the same book idempotently.
                if (books.isNotEmpty() && selectedBookId.value == null) {
                    selectBook(books.first().id)
                }
            } catch (e: Exception) {
                abError.value = e.message
            }
            abLoading.value = false
        }
    }

    fun selectBook(bookId: String) {
        viewModelScope.launch {
            selectedBookId.value = bookId
            selectedCard.value = null
            abLoading.value = true
            abError.value = null
            try {
                val fresh = api.queryCards(bookId)
                // A late reply for a book the user has since left is dropped.
                if (selectedBookId.value == bookId) cards.value = fresh
            } catch (e: Exception) {
                abError.value = e.message
                cards.value = emptyList()
            }
            abLoading.value = false
        }
    }

    fun selectCard(card: FfiCardRow) {
        selectedCard.value = card
    }

    /** Return from the card-detail pane to the card list (mobile master-detail). */
    fun clearSelectedCard() {
        selectedCard.value = null
    }
}
