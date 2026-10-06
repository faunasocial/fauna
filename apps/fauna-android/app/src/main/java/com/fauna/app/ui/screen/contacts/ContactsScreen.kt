package com.fauna.app.ui.screen.contacts

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshContainer
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.data.db.Contact
import com.fauna.app.data.db.Knock
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.GuardianAskPair
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.localized
import com.fauna.ffi.FfiAddressbookRow
import com.fauna.ffi.FfiCardRow
import com.fauna.app.ui.viewmodel.ContactOverlaysVM
import com.fauna.app.ui.viewmodel.ContactsSegment
import com.fauna.app.ui.viewmodel.ContactsVM
import com.fauna.app.ui.viewmodel.ConversationsVM
import social.fauna.generated.Ids

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ContactsScreen(
    navController: NavController,
    vm: ContactsVM = hiltViewModel()
) {
    val contacts by vm.contacts.collectAsState(initial = emptyList())
    val knocks by vm.knocks.collectAsState(initial = emptyList())
    val findResult by vm.findResult.collectAsState()
    val findError by vm.findError.collectAsState()
    val isFinding by vm.isFinding.collectAsState()
    // Address Book segment (CardDAV vCards — slice 4b, read-only master-detail).
    val segment by vm.segment.collectAsState()
    val addressbooks by vm.addressbooks.collectAsState()
    val selectedBookId by vm.selectedBookId.collectAsState()
    val cards by vm.cards.collectAsState()
    val selectedCard by vm.selectedCard.collectAsState()
    val abLoading by vm.abLoading.collectAsState()
    val abError by vm.abError.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current
    val conversationsVm: ConversationsVM = hiltViewModel()
    // Every name on this page is the shared overlay projection's — the viewer's
    // nickname where one is set (`contacts.md` § The private overlay). Re-read
    // whenever the projection moves.
    val overlaysVm: ContactOverlaysVM = hiltViewModel()
    val overlayEpoch by overlaysVm.epoch.collectAsState()

    // "Message this contact" opens the converged new-thread compose, seeded
    // with the contact's Fauna actor-id (the shared picker resolves the hex to
    // a chip). Replaces the deleted bespoke `ComposeScreen` deep-link
    // (Phase 5).
    val onMessageContact: (String) -> Unit = { actorId ->
        conversationsVm.startConversationWith(actorId)
        navController.navigate("conversation_compose")
    }

    // Tap-through: clicking a contact row opens that actor's profile (the canonical
    // list -> detail nav, profile.md § Relationship to Contacts; mirrors linux
    // app.rs open_profile). DM stays reachable via the search-result Message button
    // + the conversations compose flow.
    val onOpenProfile: (String) -> Unit = { actorId ->
        navController.navigate("profile/$actorId")
    }

    var searchInput by remember { mutableStateOf("") }
    var contactsFilter by remember { mutableStateOf("") }

    LaunchedEffect(findError) {
        appMessages.showError(findError)
    }

    // The Find User result's knock + the ward's guardian-ask pair
    // (family-safety.md § Child-initiated contact requests → *App affordance*).
    // The durable asks are collected so a status read repaints the pair.
    val knockAsk by vm.knockAsk.collectAsState()
    val wardContactRequests by vm.wardContactRequests.collectAsState()
    val knockError by vm.knockError.collectAsState()
    val askRender = remember(knockAsk, wardContactRequests) { vm.contactAskRenderFor(knockAsk) }
    LaunchedEffect(knockError) {
        appMessages.showError(knockError)
    }

    val pullToRefreshState = rememberPullToRefreshState()
    if (pullToRefreshState.isRefreshing) {
        LaunchedEffect(true) {
            vm.refresh()
            pullToRefreshState.endRefresh()
        }
    }

    LaunchedEffect(Unit) { vm.refresh() }

    // The roster filter narrows the already-loaded accepted contacts through the
    // one shared predicate, over the overlay projection — case-insensitive
    // substring over handle/domain/actor-id plus the viewer's nickname and
    // labels for that person; blank query matches all (contacts.md § The
    // private overlay → *Labels and the roster filter*).
    val groupedContacts = remember(contacts, contactsFilter, overlayEpoch) {
        val overlays = overlaysVm.overlays()
        contacts
            .filter { c -> overlays.matchesFilter(contactsFilter, c.handle, c.domain, c.peerId) }
            .groupBy { it.status }
    }

    Column(modifier = Modifier.fillMaxSize().testTag(Ids.CONTACTS_VIEW)) {

        // Contacts | Address Book segment toggle (slice 4b) — mirrors the Events
        // page's view toggle (events-view-toggle). The Address Book is a segment
        // *within* Contacts (contacts.md § Layout & flow), not a top-level page.
        SingleChoiceSegmentedButtonRow(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 8.dp)
                .testTag(Ids.CONTACTS_VIEW_SEGMENT)
        ) {
            SegmentedButton(
                selected = segment == ContactsSegment.PEOPLE,
                onClick = { vm.showPeople() },
                shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2),
                modifier = Modifier.testTag(Ids.CONTACTS_SEGMENT_PEOPLE)
            ) { Text(stringResource(R.string.common_contacts)) }
            SegmentedButton(
                selected = segment == ContactsSegment.ADDRESS_BOOK,
                onClick = { vm.showAddressBook() },
                shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2),
                modifier = Modifier.testTag(Ids.CONTACTS_SEGMENT_ADDRESSBOOK)
            ) { Text(stringResource(R.string.contacts_address_book_title)) }
        }

        if (segment == ContactsSegment.ADDRESS_BOOK) {
            AddressBookSegment(
                addressbooks = addressbooks,
                selectedBookId = selectedBookId,
                cards = cards,
                selectedCard = selectedCard,
                loading = abLoading,
                error = abError,
                onSelectBook = { vm.selectBook(it) },
                onOpenCard = { vm.selectCard(it) },
                onBack = { vm.clearSelectedCard() },
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
        } else {
    Box(
        modifier = Modifier
            .weight(1f)
            .fillMaxWidth()
            .nestedScroll(pullToRefreshState.nestedScrollConnection)
    ) {
    LazyColumn(modifier = Modifier.fillMaxSize()) {

        // ── Section 1: Find User ─────────────────────────────────────────────
        item {
            Column(modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
                Text(
                    text = stringResource(R.string.contacts_find_user_title),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(bottom = 8.dp)
                )
                OutlinedTextField(
                    value = searchInput,
                    onValueChange = { searchInput = it },
                    placeholder = { Text(stringResource(R.string.contacts_find_user_placeholder)) },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.CONTACT_ACTOR_ID_FIELD),
                    singleLine = true
                )
                Spacer(modifier = Modifier.height(8.dp))
                Button(
                    onClick = { vm.findUser(searchInput) },
                    enabled = searchInput.isNotBlank() && !isFinding,
                    modifier = Modifier.align(Alignment.End).testTag(Ids.CONTACT_ACTOR_ID_LOOKUP)
                ) {
                    if (isFinding) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(16.dp),
                            strokeWidth = 2.dp,
                            color = MaterialTheme.colorScheme.onPrimary
                        )
                    } else {
                        Text(stringResource(R.string.contacts_find_user_find))
                    }
                }

                findResult?.let { resolved ->
                    Spacer(modifier = Modifier.height(8.dp))
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(12.dp)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    text = resolved.actorId,
                                    style = MaterialTheme.typography.bodySmall,
                                    overflow = TextOverflow.Ellipsis,
                                    maxLines = 1,
                                    modifier = Modifier
                                        .weight(1f)
                                        .testTag(Ids.CONTACT_ACTOR_ID_RESULT)
                                )
                                CopyButton(
                                    testTag = Ids.CONTACT_ACTOR_ID_COPY_BTN,
                                    text = resolved.actorId,
                                    onCopied = {
                                        appMessages.showInfo(context.getString(R.string.settings_account_page_copied_clipboard))
                                    },
                                )
                            }
                            if (resolved.handle != null && resolved.domain != null) {
                                Text(
                                    text = "${resolved.handle}@${resolved.domain}",
                                    style = MaterialTheme.typography.bodyMedium
                                )
                            }
                            Spacer(modifier = Modifier.height(8.dp))
                            Row(
                                modifier = Modifier.align(Alignment.End),
                                horizontalArrangement = Arrangement.spacedBy(8.dp)
                            ) {
                                // "Sent" only once the nest accepted the knock
                                // (tui's `knock_sent`), never optimistically.
                                val sent = knockAsk.peer == resolved.actorId && knockAsk.knockSent
                                val inFlight = knockAsk.peer == resolved.actorId && knockAsk.knockInFlight
                                Button(
                                    onClick = { vm.addContact(resolved.actorId) },
                                    enabled = !sent && !inFlight,
                                    modifier = Modifier.testTag(Ids.CONTACTS_ADD_BUTTON)
                                ) {
                                    Text(stringResource(if (sent) R.string.contacts_sent else R.string.contacts_knock))
                                }
                                Button(
                                    onClick = { onMessageContact(resolved.actorId) }
                                ) {
                                    Text(stringResource(R.string.contacts_message))
                                }
                            }
                            // The pair belongs to the peer this lookup found
                            // (`knockAsk.peer`), never a previous result's.
                            if (knockAsk.peer == resolved.actorId) {
                                GuardianAskPair(
                                    render = askRender,
                                    askInFlight = knockAsk.askInFlight,
                                    onAsk = { vm.askGuardian(resolved.actorId) },
                                    modifier = Modifier.align(Alignment.End).padding(top = 8.dp),
                                )
                            }
                        }
                    }
                }

            }
        }

        item { HorizontalDivider() }

        // ── Section 2: Message Requests (Knocks) ─────────────────────────────
        if (knocks.isNotEmpty()) {
            item {
                Text(
                    text = stringResource(R.string.contacts_message_requests_title),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
                )
            }

            items(knocks, key = { it.id }) { knock ->
                // A nickname if the viewer has given this sender one, else
                // the canonical short id.
                val sender = remember(knock.sender, overlayEpoch) {
                    overlaysVm.overlays().peerLabel(null, null, knock.sender).primary
                }
                KnockItem(knock = knock, senderLabel = sender, vm = vm)
                HorizontalDivider()
            }
        }

        // ── Section 3: Contacts List ─────────────────────────────────────────
        item {
            OutlinedTextField(
                value = contactsFilter,
                onValueChange = { contactsFilter = it },
                placeholder = { Text(stringResource(R.string.common_search)) },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp)
                    .testTag(Ids.CONTACTS_SEARCH_FIELD),
                singleLine = true
            )
        }

        val statusOrder = listOf("confirmed", "accepted", "pending", "blocked")

        for (status in statusOrder) {
            val group = groupedContacts[status] ?: continue
            if (group.isEmpty()) continue

            item(key = "header_$status") {
                Text(
                    // Status label single-sourced in shared Rust
                    // (fauna_core::format::contact_status_label via UniFFI), not a
                    // local capitalize — contacts.md § Where logic lives.
                    text = localized(com.fauna.ffi.contactStatusLabel(status)) ?: status,
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
                )
            }

            items(group, key = { it.peerId }) { contact ->
                val overlays = remember(overlayEpoch) { overlaysVm.overlays() }
                val label = remember(contact, overlayEpoch) {
                    overlays.peerLabel(null, contact.handle, contact.peerId)
                }
                val labelsLine = remember(contact.peerId, overlayEpoch) {
                    overlays.labelsLine(contact.peerId)
                }
                ContactItem(
                    contact = contact,
                    primary = label.primary,
                    publicName = label.public,
                    labelsLine = labelsLine,
                    vm = vm,
                    onOpenProfile = onOpenProfile,
                )
                HorizontalDivider()
            }
        }
    }
    PullToRefreshContainer(
        state = pullToRefreshState,
        modifier = Modifier.align(Alignment.TopCenter)
    )
    } // end Box
        } // end else (people segment)
    } // end Column
}

// ── Address Book segment (CardDAV vCards — slice 4b, read-only) ──────────────────
//
// The Contacts page's second segment: a read-only master-detail over the actor's
// OWN CardDAV address books + vCards, decrypted locally (carddav-server.md
// § Independent enablement). Mirrors the web/linux 3-pane reference, adapted to
// the phone form factor as book-chips → card-list → detail (the Events page's
// mobile master-detail idiom). All parsing/unseal lives in the shared crate; the
// FfiCardRow / FfiAddressbookRow records are already the display model.
@Composable
private fun AddressBookSegment(
    addressbooks: List<FfiAddressbookRow>,
    selectedBookId: String?,
    cards: List<FfiCardRow>,
    selectedCard: FfiCardRow?,
    loading: Boolean,
    error: String?,
    onSelectBook: (String) -> Unit,
    onOpenCard: (FfiCardRow) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // Fallback name for a book with no sealed displayname (hoisted out of the
    // LazyRow's non-composable LazyListScope).
    val untitled = stringResource(R.string.contacts_address_book_title)
    Column(modifier = modifier) {
        // Book picker (horizontal chips — mirror the Events calendar-item row).
        if (addressbooks.isEmpty()) {
            Text(
                text = if (loading) stringResource(R.string.common_loading)
                       else stringResource(R.string.contacts_address_book_no_addressbooks),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(16.dp)
            )
        } else {
            LazyRow(
                contentPadding = PaddingValues(horizontal = 16.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                items(addressbooks, key = { it.id }) { book ->
                    val name = book.name.ifBlank { untitled }
                    FilterChip(
                        selected = selectedBookId == book.id,
                        onClick = { onSelectBook(book.id) },
                        label = { Text("$name (${book.cardCount})") },
                        modifier = Modifier.testTag(Ids.ADDRESSBOOK_ITEM)
                    )
                }
            }
        }

        HorizontalDivider()

        // Master-detail: the selected card's detail replaces the card list (the
        // phone master-detail swap), with a Back affordance to return.
        val card = selectedCard
        if (card != null) {
            CardDetail(
                card = card,
                onBack = onBack,
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
        } else if (selectedBookId != null && cards.isEmpty()) {
            Text(
                text = if (loading) stringResource(R.string.common_loading)
                       else stringResource(R.string.contacts_address_book_no_cards),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(16.dp)
            )
        } else {
            LazyColumn(modifier = Modifier.weight(1f).fillMaxWidth()) {
                items(cards, key = { it.id }) { c ->
                    ListItem(
                        headlineContent = {
                            Text(
                                c.formattedName,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.testTag(Ids.VCARD_CARD_FN)
                            )
                        },
                        modifier = Modifier
                            .testTag(Ids.VCARD_CARD)
                            .clickable { onOpenCard(c) }
                    )
                    HorizontalDivider()
                }
            }
        }

        error?.let {
            Text(
                text = it,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
            )
        }
    }
}

// The vCard detail pane (ui.yaml card_detail sub-page): FN header + ORG + one row
// per EMAIL / TEL / ADR + NOTE. Read-only; the card is already decoded, so no
// round-trip. Mirrors the linux build_card_detail ordering.
@Composable
private fun CardDetail(card: FfiCardRow, onBack: () -> Unit, modifier: Modifier = Modifier) {
    Column(
        modifier = modifier
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 8.dp)
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
            }
            Text(
                text = card.formattedName,
                style = MaterialTheme.typography.headlineSmall,
                modifier = Modifier.testTag(Ids.VCARD_DETAIL_FN)
            )
        }
        if (card.title.isNotBlank()) {
            Text(
                text = card.title,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 4.dp)
            )
        }
        // ORG components joined (single line — matches the web `·`-join).
        val org = card.org.filter { it.isNotBlank() }.joinToString(" · ")
        if (org.isNotBlank()) {
            Text(
                text = org,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 4.dp).testTag(Ids.VCARD_DETAIL_ORG)
            )
        }
        Spacer(modifier = Modifier.height(8.dp))
        card.emails.forEach {
            VCardDetailRow(stringResource(R.string.contacts_address_book_email), it.value, Ids.VCARD_DETAIL_EMAIL)
        }
        card.tels.forEach {
            VCardDetailRow(stringResource(R.string.contacts_address_book_phone), it.value, Ids.VCARD_DETAIL_TEL)
        }
        card.addresses.forEach {
            VCardDetailRow(stringResource(R.string.contacts_address_book_address), it.formatted, Ids.VCARD_DETAIL_ADR)
        }
        if (card.note.isNotBlank()) {
            VCardDetailRow(stringResource(R.string.contacts_address_book_note), card.note, Ids.VCARD_DETAIL_NOTE)
        }
    }
}

// One labeled detail row: a dim field label + the value tagged for get_text.
@Composable
private fun VCardDetailRow(label: String, value: String, valueTestTag: String) {
    Row(
        modifier = Modifier.padding(vertical = 4.dp, horizontal = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp)
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.width(90.dp)
        )
        Text(
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.weight(1f).testTag(valueTestTag)
        )
    }
}

@Composable
private fun KnockItem(knock: Knock, senderLabel: String, vm: ContactsVM) {
    val context = LocalContext.current
    ListItem(
        headlineContent = {
            Text(senderLabel, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.testTag(Ids.KNOCK_SENDER))
        },
        supportingContent = {
            Column {
                Text(
                    text = knock.senderNode,
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis
                )
                Text(
                    text = knock.summary,
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis
                )
                Text(
                    text = ValueFormat.relativeTime(context, knock.createdAt),
                    style = MaterialTheme.typography.labelSmall
                )
                Row(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.padding(top = 4.dp)
                ) {
                    TextButton(onClick = { vm.acceptKnock(knock.sender) },
                        modifier = Modifier.testTag(Ids.CONTACTS_ACCEPT_BUTTON)) {
                        Text(stringResource(R.string.common_accept))
                    }
                    TextButton(onClick = { vm.dismissKnock(knock.sender) },
                        modifier = Modifier.testTag(Ids.KNOCK_DISMISS)) {
                        Text(stringResource(R.string.common_dismiss))
                    }
                    TextButton(onClick = { vm.blockKnock(knock.sender) },
                        modifier = Modifier.testTag(Ids.CONTACTS_BLOCK_BUTTON)) {
                        Text(stringResource(R.string.common_block), color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        },
        modifier = Modifier.testTag(Ids.KNOCK_CARD)
    )
}

@Composable
private fun ContactItem(
    contact: Contact,
    primary: String,
    publicName: String?,
    labelsLine: String?,
    vm: ContactsVM,
    onOpenProfile: (String) -> Unit,
) {
    // The post-succession review badge — the second of § Propagation's three
    // renderings of the one flag (the conversations-page member chip pair is
    // the load-bearing one; the permanent Settings view is the third). Badge
    // only, deliberately with no Keep/Remove pair: the decision belongs where
    // removal already lives (the group member chip), and this row has no
    // eviction affordance to join (identity-succession.md § Propagation →
    // *MLS groups*). Compares by bytes, mirroring linux's
    // `contact_under_review` (an unparseable peerId is not under review).
    val memberReviewRoster by vm.memberReviewRoster.collectAsState()
    val reviewed = remember(contact.peerId, memberReviewRoster) {
        runCatching { HexUtil.hexToBytes(contact.peerId) }.getOrNull()?.let { personBytes ->
            memberReviewRoster.any { it.person.contentEquals(personBytes) }
        } ?: false
    }

    val statusColor = when (contact.status) {
        "confirmed" -> MaterialTheme.colorScheme.primary
        "accepted" -> Color(0xFF22C55E)
        "pending" -> Color(0xFFF59E0B)
        "blocked" -> MaterialTheme.colorScheme.error
        else -> MaterialTheme.colorScheme.outline
    }

    // Fauna Kids opens the profile of an approved (`confirmed`) contact only —
    // profiles of non-contacts are excised (family-safety.md § The account age
    // band, the kids-app bullet, item (4)), and this row is the one route to
    // another actor's profile in that flavor (search and the feed are compiled out).
    val clickable = contact.status != "blocked" && (!BuildConfig.KIDS || contact.status == "confirmed")

    ContactRowContent(
        primary = primary,
        publicName = publicName,
        labelsLine = labelsLine,
        onClick = if (clickable) ({ onOpenProfile(contact.peerId) }) else null,
        trailing = {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                if (contact.status == "accepted") {
                    TextButton(onClick = { vm.confirmContact(contact.peerId) },
                        modifier = Modifier.testTag(Ids.CONTACT_CONFIRM)) {
                        Text(stringResource(R.string.common_confirm))
                    }
                }
                if (reviewed) {
                    StatusBadge(
                        label = stringResource(R.string.contacts_unattested_mark),
                        color = Color(0xFFF59E0B),
                        modifier = Modifier.testTag(Ids.CONTACT_UNATTESTED_MARK),
                    )
                }
                // Same shared label as the section header; color stays per-platform.
                StatusBadge(
                    label = localized(com.fauna.ffi.contactStatusLabel(contact.status)) ?: contact.status,
                    color = statusColor,
                )
            }
        },
    )
}

/**
 * One roster row's names (`contacts.md` § The private overlay → *Where the
 * nickname paints*): [primary] on `contact-name`; while a nickname is the
 * primary line, the public name it replaced on `contact-public-name`; and the
 * person's labels on one `contact-labels` line. The two conditional lines are
 * absent, not empty, when there is nothing to show, and both sit inside the
 * `contact-row` container so a scoped read finds them. Stateless for the
 * Robolectric harness — every string is the shared projection's.
 */
@Composable
fun ContactRowContent(
    primary: String,
    publicName: String?,
    labelsLine: String?,
    onClick: (() -> Unit)?,
    trailing: @Composable () -> Unit = {},
) {
    ListItem(
        headlineContent = {
            Text(primary, maxLines = 1, overflow = TextOverflow.Ellipsis,
                modifier = Modifier.testTag(Ids.CONTACT_NAME))
        },
        supportingContent = if (publicName == null && labelsLine == null) null else ({
            Column {
                if (publicName != null) {
                    Text(publicName, maxLines = 1, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.testTag(Ids.CONTACT_PUBLIC_NAME))
                }
                if (labelsLine != null) {
                    Text(labelsLine, maxLines = 1, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.testTag(Ids.CONTACT_LABELS))
                }
            }
        }),
        trailingContent = trailing,
        modifier = if (onClick != null) {
            Modifier.testTag(Ids.CONTACT_ROW).clickable(onClick = onClick)
        } else {
            Modifier.testTag(Ids.CONTACT_ROW)
        }
    )
}

@Composable
private fun StatusBadge(label: String, color: Color, modifier: Modifier = Modifier.testTag(Ids.CONTACT_STATUS)) {
    Surface(
        shape = RoundedCornerShape(4.dp),
        color = color.copy(alpha = 0.15f),
        modifier = modifier
    ) {
        Text(
            text = label,
            color = color,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp)
        )
    }
}
