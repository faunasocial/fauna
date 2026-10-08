package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccountCircle
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.core.ContactAskRender
import com.fauna.app.ui.components.GuardianAskPair
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.ContactOverlaysVM
import com.fauna.app.ui.viewmodel.ConversationsVM
import com.fauna.app.ui.viewmodel.ProfilePrivateVM
import com.fauna.app.ui.viewmodel.ProfileVM
import social.fauna.generated.Ids

/** Which Profile tab is showing. Posts is the default landmark (content TBD). */
private enum class ProfileTab { POSTS, TIERS }

/**
 * The Profile page — the canonical per-user *detail* surface (`profile.md`). It
 * renders **either** the viewer's own profile (SELF, the top-level `profile-tab`
 * drawer row) **or** another actor's (OTHER, reached by tap-through — a contact
 * row → `profile/{actorId}`), branching on [ProfileVM.isSelf] (mirrors linux
 * `build_profile_view(target)`). Both show an identity header + a tab strip
 * (`profile-posts-tab` landmark · `profile-tiers-tab`).
 *
 * The header renders the viewed actor's published **display_name**
 * (`fauna.profile.get`, via [ProfileVM.refreshHeader]), falling back to the
 * handle/actor_id + an avatar placeholder. The primary action is
 * `profile-edit-button` (SELF → the text-only `profile-edit-form`
 * [ProfileEditFormSection], publishes via `fauna.profile.set` + refreshes the
 * header) or `profile-follow-button` (OTHER → follow = subscribe to the free
 * "followers" tier). The Tiers tab hosts the SELF author management
 * ([ProfileTiersSection]) or the OTHER subscriber-browse offers
 * ([ProfileOffersSection]). `page-heading` and `error-message` come from the
 * global app chrome (TopAppBar + MessageBanner) — Tiers/offers/edit/follow errors
 * ride `LocalAppMessages`. Lifts the linux lead (apps/fauna-linux/src/views/profile/).
 */
@Composable
fun ProfileScreen(
    navController: NavController,
    vm: ProfileVM = hiltViewModel(),
    conversationsVm: ConversationsVM = hiltViewModel(),
    overlaysVm: ContactOverlaysVM = hiltViewModel(),
    privateVm: ProfilePrivateVM = hiltViewModel(),
    reportVm: com.fauna.app.ui.viewmodel.ReportVM = hiltViewModel(),
) {
    var tab by remember { mutableStateOf(ProfileTab.POSTS) }
    // Bumped by every `profile-tiers-tab` activation and passed down as a
    // `LaunchedEffect` key, so the Tiers branch re-reads even when it is already
    // composed — the ruled uniform door (`monetization.md` § Pillar 1 → *The
    // Tiers-tab re-read door*). Compose gives the branch no other way to hear a
    // click that does not change `tab`.
    var tiersReloadToken by remember { mutableIntStateOf(0) }
    var editing by remember { mutableStateOf(false) }
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    val displayName by vm.displayName.collectAsState()
    // OTHER: the header's two names are the shared resolver's over the viewer's
    // nickname, the published display name, then the canonical short id — the
    // OTHER header holds no handle (mirrors linux `HeaderNames::label`). SELF
    // keeps its own one-line name. Re-read whenever the projection moves.
    val publishedName by vm.publishedName.collectAsState()
    val overlayEpoch by overlaysVm.epoch.collectAsState()
    val otherLabel = remember(publishedName, overlayEpoch) {
        vm.profileActorIdHex?.takeIf { !vm.isSelf }?.let {
            overlaysVm.overlays().peerLabel(publishedName, null, it)
        }
    }
    val privateForm by privateVm.form.collectAsState()
    val privateSaving by privateVm.saving.collectAsState()
    val privateError by privateVm.error.collectAsState()
    // An untouched section follows a sibling device's edit.
    LaunchedEffect(overlayEpoch) { privateVm.reload() }
    val following by vm.following.collectAsState()
    val followWorking by vm.followWorking.collectAsState()
    val blocked by vm.blocked.collectAsState()
    val blockWorking by vm.blockWorking.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    // OTHER: the knock + the ward's guardian-ask pair; the durable asks are
    // collected so a status read repaints the pair (rule (c)).
    val knockAsk by vm.knockAsk.collectAsState()
    val wardContactRequests by vm.wardContactRequests.collectAsState()
    val askRender = remember(knockAsk, wardContactRequests) { vm.contactAskRenderFor(knockAsk) }

    LaunchedEffect(Unit) {
        vm.refreshHeader()
        // OTHER profile: seed the Block⇄Unblock toggle from the current edge.
        if (!vm.isSelf) vm.refreshBlockState()
    }
    // A page error shows on `error-message`; when this page's own error is
    // retired (a landed knock or guardian ask retires the refusal) the banner
    // clears — but only if it still shows THIS page's text, never a sibling
    // section's.
    var shownError by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(errorMessage) {
        val msg = errorMessage
        if (msg != null) {
            appMessages.showError(msg)
        } else if (shownError != null && appMessages.error.value == shownError) {
            appMessages.showError(null)
        }
        shownError = msg
    }
    // The private section's refusals ride the same `error-message`; a landed
    // Save clears only its own text.
    var shownPrivateError by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(privateError) {
        val msg = privateError
        if (msg != null) {
            appMessages.showError(msg)
        } else if (shownPrivateError != null && appMessages.error.value == shownPrivateError) {
            appMessages.showError(null)
        }
        shownPrivateError = msg
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .testTag(Ids.PROFILE_VIEW),
    ) {
        // ── Identity header (the shared user-header shape) ──────────────────
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Icon(
                Icons.Filled.AccountCircle,
                contentDescription = null,
                modifier = Modifier.size(48.dp),
                tint = MaterialTheme.colorScheme.primary,
            )
            ProfileHeaderNames(
                primary = otherLabel?.primary ?: displayName,
                publicName = otherLabel?.public,
                modifier = Modifier.weight(1f),
            )
            // The button's `copied` attribute (its `stateDescription`) is the exact
            // string handed to the clipboard, never re-derived — the driver-facing
            // witness of a copy, since no driver reads the OS clipboard. Keyed on
            // the viewed actor, so every profile open starts it empty.
            var copiedId by remember(vm.profileActorIdHex) { mutableStateOf("") }
            OutlinedButton(
                onClick = {
                    vm.profileActorIdHex?.let {
                        clipboard.setText(AnnotatedString(it))
                        copiedId = it
                    }
                },
                modifier = Modifier
                    .testTag(Ids.PROFILE_ACTOR_ID_COPY_BTN)
                    .semantics { stateDescription = copiedId },
            ) { Text(stringResource(R.string.profile_copy_id)) }
            if (vm.isSelf) {
                // SELF: profile-edit-button opens the text-only edit form (profile.md
                // § Where logic lives → Profile publish/edit); save publishes + refreshes.
                OutlinedButton(
                    onClick = { editing = true },
                    modifier = Modifier.testTag(Ids.PROFILE_EDIT_BUTTON),
                ) { Text(stringResource(R.string.profile_edit)) }
            } else if (!BuildConfig.KIDS) {
                // OTHER: profile-follow-button = subscribe to the free "followers" tier.
                // Not in Fauna Kids: following is a subscription (monetization),
                // excised with the Tiers tab below (family-safety.md § The account
                // age band, the kids-app bullet, item (4)).
                OutlinedButton(
                    onClick = { vm.follow() },
                    enabled = !followWorking && !following,
                    modifier = Modifier.testTag(Ids.PROFILE_FOLLOW_BUTTON),
                ) {
                    Text(resolveLocalized(context, com.fauna.ffi.followToggleLabel(following)).orEmpty())
                }
            }
        }

        // ── OTHER: secondary relationship actions (start DM + block toggle) ───
        if (!vm.isSelf) {
            ProfileSecondaryActionsRow(
                blocked = blocked,
                blockWorking = blockWorking,
                onStartDm = {
                    // Pure nav glue: seed the shared Conversations new-thread
                    // composer with the viewed actor and open it (mirrors the
                    // Contacts "message" deep-link + linux start_dm).
                    vm.profileActorIdHex?.let { hex ->
                        conversationsVm.startConversationWith(hex)
                        navController.navigate("conversation_compose")
                    }
                },
                onToggleBlock = { vm.toggleBlock() },
                blockLabel = { isBlocked ->
                    resolveLocalized(context, com.fauna.ffi.contactToggleBlockLabel(isBlocked)).orEmpty()
                },
                requestContactSent = knockAsk.knockSent,
                requestContactInFlight = knockAsk.knockInFlight,
                onRequestContact = { vm.requestContact() },
                askRender = askRender,
                askInFlight = knockAsk.askInFlight,
                onAskGuardian = { vm.askGuardian() },
                // Report account — opens the shared report sheet on this OTHER
                // profile (moderation.md § User-initiated reporting → *App
                // surface*; the shell's ReportHost paints it). An account has no
                // text to attach, so the shared target offers no include-text box.
                onReport = {
                    vm.profileActorIdHex?.let { hex ->
                        reportVm.open(com.fauna.ffi.reportActorTarget(hex))
                    }
                },
            )
            // ── OTHER: the private section (nickname, notes, labels) ─────────
            ProfilePrivateSection(
                nickname = privateForm.nickname,
                notes = privateForm.notes,
                labels = privateForm.labels,
                saving = privateSaving,
                onNicknameChange = privateVm::setNickname,
                onNotesChange = privateVm::setNotes,
                onAddLabel = privateVm::addLabel,
                onRemoveLabel = privateVm::removeLabel,
                onSave = privateVm::save,
            )
        }

        // ── Edit form (revealed by profile-edit-button; SELF publish) ─────────
        if (vm.isSelf && editing) {
            ProfileEditFormSection(
                actorIdHex = vm.ownActorIdHex,
                onSaved = { editing = false; vm.refreshHeader() },
                onCancel = { editing = false },
            )
        }

        // ── Tab strip ───────────────────────────────────────────────────────
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            FilterChip(
                selected = tab == ProfileTab.POSTS,
                onClick = { tab = ProfileTab.POSTS },
                label = { Text(stringResource(R.string.profile_posts)) },
                modifier = Modifier.testTag(Ids.PROFILE_POSTS_TAB),
            )
            if (!BuildConfig.KIDS) {
                FilterChip(
                    selected = tab == ProfileTab.TIERS,
                    onClick = { tab = ProfileTab.TIERS; tiersReloadToken++ },
                    label = { Text(stringResource(R.string.profile_tiers)) },
                    modifier = Modifier.testTag(Ids.PROFILE_TIERS_TAB),
                )
            }
        }

        when (tab) {
            ProfileTab.POSTS -> Text(
                stringResource(R.string.profile_no_posts),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(16.dp),
            )
            ProfileTab.TIERS ->
                if (BuildConfig.KIDS) {
                    // Unreachable: the Tiers chip is not composed in Fauna Kids.
                } else if (vm.isSelf) {
                    ProfileTiersSection(reloadToken = tiersReloadToken)
                } else {
                    ProfileOffersSection(
                        targetActorIdHex = vm.profileActorIdHex.orEmpty(),
                        reloadToken = tiersReloadToken,
                    )
                }
        }
    }
}

/**
 * OTHER-profile secondary relationship actions row (`profile.md` § Layout & flow):
 * `profile-start-dm-button` (open the Conversations new-thread composer pre-filled
 * with the viewed actor — pure nav glue, no new kind/persistence) +
 * `profile-block-button` (the Block ⇄ Unblock toggle whose label flips on the
 * viewed actor's `contact_status`) + `profile-request-contact-button` (the knock,
 * routed by the shared `knockRecipientNestUrl` — `profile.md` § Where logic
 * lives → *Request contact routing*) with the supervised ward's guardian-ask
 * pair beside it ([GuardianAskPair]; family-safety.md § Child-initiated contact
 * requests → *App affordance*). Stateless for the Robolectric harness (no VM /
 * FFI); mirrors tui's `profile/mod.rs` and the linux `views/profile/mod.rs`.
 */
@Composable
fun ProfileSecondaryActionsRow(
    blocked: Boolean,
    blockWorking: Boolean,
    onStartDm: () -> Unit,
    onToggleBlock: () -> Unit,
    // The Block ⇄ Unblock wording comes from the shared `contact_toggle_block_label`
    // map, resolved by the stateful caller and injected so this Content stays FFI-free
    // for the Robolectric harness (mirrors `member_status_label` / `bridge_display_name`).
    blockLabel: (Boolean) -> String,
    modifier: Modifier = Modifier,
    // The knock: "Request sent" + disabled once the nest accepted it (never
    // optimistically); disabled while in flight.
    requestContactSent: Boolean = false,
    requestContactInFlight: Boolean = false,
    onRequestContact: () -> Unit = {},
    // The ward's ask pair, computed by the stateful caller (durable pending
    // first, the ask only after the TYPED refusal); `null` paints nothing.
    askRender: ContactAskRender? = null,
    askInFlight: Boolean = false,
    onAskGuardian: () -> Unit = {},
    // `profile-report-button` (moderation.md § User-initiated reporting): `null`
    // paints nothing; the stateful caller supplies it for an OTHER profile only.
    onReport: (() -> Unit)? = null,
) {
    Column(modifier = modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(
                onClick = onStartDm,
                modifier = Modifier.testTag(Ids.PROFILE_START_DM_BUTTON),
            ) { Text(stringResource(R.string.profile_start_dm)) }

            OutlinedButton(
                onClick = onToggleBlock,
                enabled = !blockWorking,
                modifier = Modifier.testTag(Ids.PROFILE_BLOCK_BUTTON),
            ) {
                Text(blockLabel(blocked))
            }

            if (onReport != null) {
                OutlinedButton(
                    onClick = onReport,
                    modifier = Modifier.testTag(Ids.PROFILE_REPORT_BUTTON),
                ) { Text(stringResource(R.string.profile_report)) }
            }
        }
        // Second line: the knock and, beside it, the ward's ask pair (a phone-width
        // row cannot hold four actions).
        Row(
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedButton(
                onClick = onRequestContact,
                enabled = !requestContactSent && !requestContactInFlight,
                modifier = Modifier.testTag(Ids.PROFILE_REQUEST_CONTACT_BUTTON),
            ) {
                Text(
                    stringResource(
                        if (requestContactSent) R.string.profile_request_contact_sent
                        else R.string.profile_request_contact,
                    ),
                )
            }
            GuardianAskPair(render = askRender, askInFlight = askInFlight, onAsk = onAskGuardian)
        }
    }
}
