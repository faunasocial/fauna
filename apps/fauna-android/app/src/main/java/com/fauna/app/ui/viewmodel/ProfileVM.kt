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
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.classifyKnock
import com.fauna.app.core.contactAskRender
import com.fauna.app.core.foldContactAskFailed
import com.fauna.app.core.foldContactAsked
import com.fauna.app.core.foldKnockReply
import com.fauna.ffi.actorIdFromSecret
import com.fauna.ffi.knockRecipientNestUrl
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The Profile shell's identity, for **either** the viewer's own profile (SELF,
 * the `profile-tab` drawer route, no `actorId` arg) **or** another actor's
 * (OTHER, the `profile/{actorId}` route reached by tap-through) — branches on
 * [isSelf], mirroring linux `build_profile_view(target)` (`profile.md` § Layout
 * & flow).
 *
 * The header renders the published **display_name** (`fauna.profile.get` →
 * shared `decode_profile`), falling back to — SELF: the cached handle → own
 * actor_id; OTHER: the target actor_id ([refreshHeader], on mount; mirrors linux
 * `refresh_header_name`). On an OTHER profile the primary action is
 * [follow] = subscribe to the free "followers" tier.
 */
@HiltViewModel
class ProfileVM @Inject constructor(
    savedStateHandle: SavedStateHandle,
    private val secureStorage: SecureStorage,
    private val api: ApiClient,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    /** The viewed actor: the route `actorId` arg (OTHER), or `null` = the viewer's own (SELF). */
    private val targetActorId: String? = savedStateHandle.get<String>("actorId")

    /** True when showing the viewer's own profile (the `profile-tab` drawer route). */
    val isSelf: Boolean = targetActorId == null

    /** Hex actor_id of the viewer, derived locally from the stored secret. */
    val ownActorIdHex: String? =
        secureStorage.secretHex?.let {
            try {
                HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it)))
            } catch (_: Exception) {
                null
            }
        }

    /** The actor whose profile is shown: the target (OTHER) or the viewer (SELF). */
    val profileActorIdHex: String? = targetActorId ?: ownActorIdHex

    /** Header fallback when no profile is published: SELF → cached handle → own actor_id; OTHER → target actor_id. */
    private val fallbackName: String =
        if (isSelf) {
            secureStorage.handle?.takeIf { it.isNotBlank() } ?: ownActorIdHex ?: ""
        } else {
            targetActorId ?: ""
        }

    private val _displayName = MutableStateFlow(fallbackName)

    /** Header label: the published display_name once [refreshHeader] returns, else the fallback. */
    val displayName: StateFlow<String> = _displayName.asStateFlow()

    private val _publishedName = MutableStateFlow<String?>(null)

    /**
     * The published display_name alone (`null` until [refreshHeader] lands or
     * when none is published) — what an OTHER header hands the shared resolver
     * as the person's self-published name (`contacts.md` § The private overlay
     * → *Where the nickname paints*); the resolver, not this class, answers the
     * fallback.
     */
    val publishedName: StateFlow<String?> = _publishedName.asStateFlow()

    /** True once the viewer has followed this actor (OTHER only) — flips the button label. */
    private val _following = MutableStateFlow(false)
    val following: StateFlow<Boolean> = _following.asStateFlow()

    /** True while a [follow] round-trip is in flight (disables the follow button). */
    private val _followWorking = MutableStateFlow(false)
    val followWorking: StateFlow<Boolean> = _followWorking.asStateFlow()

    /** True when the viewed actor's contact edge is `blocked` (OTHER only) — flips
     *  the `profile-block-button` label Block ⇄ Unblock. */
    private val _blocked = MutableStateFlow(false)
    val blocked: StateFlow<Boolean> = _blocked.asStateFlow()

    /** True while a [toggleBlock] round-trip is in flight (disables the toggle). */
    private val _blockWorking = MutableStateFlow(false)
    val blockWorking: StateFlow<Boolean> = _blockWorking.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    /**
     * OTHER only: `profile-request-contact-button`'s knock + the supervised
     * ward's guardian-ask pair (family-safety.md § Child-initiated contact
     * requests → *App affordance*; tui's `profile/mod.rs` `knock_sent` /
     * `guardian_refused` / `contact_ask_sent`). Per open, bound to the viewed
     * actor; replies fold through [foldKnockReply] keyed on the peer they were
     * SENT to, so a reply can only ever paint on its own actor's open (rule (g)).
     */
    private val _knockAsk = MutableStateFlow(KnockAskState.forPeer(targetActorId))
    val knockAsk: StateFlow<KnockAskState> = _knockAsk.asStateFlow()

    /** The ward's durable own asks — collected by the screen so the pair
     *  repaints when a status read lands (rule (c)). */
    val wardContactRequests = api.wardAsks.contactRequests

    /**
     * Where this open's knock goes: the shared `knockRecipientNestUrl` over the
     * profile [refreshHeader] already fetched (`profile.md` § Where logic lives
     * → *Request contact routing*). `null` until that read lands, and whenever
     * the profile gives no better answer — the knock then goes to this nest.
     */
    @Volatile
    private var knockRoute: String? = null

    /**
     * Re-read the viewed actor's published `display_name` (`fauna.profile.get`)
     * and render it into the header, falling back to the handle/actor_id on
     * `not_found`/error. Called on mount and after a successful SELF edit-form
     * publish. Mirrors linux `mod.rs::refresh_header_name` (works for any actor).
     *
     * OTHER: the same read fills [knockRoute] — the profile the header renders
     * is the one whose home nest a knock from this page goes to.
     */
    fun refreshHeader() {
        val hex = profileActorIdHex ?: return
        viewModelScope.launch {
            val body = api.profileGet(hex)
            val published = body
                ?.let { runCatching { api.decodeProfileDisplay(it).displayName }.getOrNull() }
                ?.takeIf { it.isNotBlank() }
            _publishedName.value = published
            _displayName.value = published ?: fallbackName
            if (targetActorId != null) {
                knockRoute = body?.let {
                    runCatching { knockRecipientNestUrl(HexUtil.hexToBytes(targetActorId), it, api.nodeUrl) }.getOrNull()
                }
            }
        }
    }

    /** The pair's render — durable pending first (rule (c)); `null` on SELF. */
    fun contactAskRenderFor(state: KnockAskState): ContactAskRender? {
        val peer = state.peer ?: return null
        return contactAskRender(api.wardAsks.contactAskPending(peer), state)
    }

    /**
     * `profile-request-contact-button` — knock the viewed actor on the route the
     * open-time read took from its own profile. Reads "Request sent" and stays
     * disabled once the nest accepted it; the TYPED guardian refusal
     * re-enables it, offers the ask, and STAYS on `error-message` (rules (a),
     * (b)). Mirrors tui's `Action::RequestContact`.
     */
    fun requestContact() {
        val peer = targetActorId ?: return
        val before = _knockAsk.value
        if (before.knockSent || before.knockInFlight) return
        val actorId = ownActorIdHex ?: return
        _knockAsk.value = before.copy(knockInFlight = true)
        val route = knockRoute
        viewModelScope.launch {
            val result = classifyKnock { api.sendKnock(actorId, peer, route) }
            val (next, effect) = foldKnockReply(_knockAsk.value, peer, result)
            _knockAsk.value = next
            errorMessage.value = when (effect) {
                KnockErrorEffect.Clear -> null
                KnockErrorEffect.Guardian -> context.getString(R.string.contacts_guardian_approval_required)
                is KnockErrorEffect.Failed -> effect.message
            }
        }
    }

    /** `contact-request-guardian-button` — ask the guardian about the viewed
     *  actor (`fauna.family.contact.request`); a pending ask is not re-sent. */
    fun askGuardian() {
        val peer = targetActorId ?: return
        val before = _knockAsk.value
        if (before.askInFlight || contactAskRenderFor(before) == ContactAskRender.PENDING) return
        _knockAsk.value = before.copy(askInFlight = true)
        viewModelScope.launch {
            try {
                api.familyContactRequest(peer)
                _knockAsk.value = foldContactAsked(_knockAsk.value, peer)
                errorMessage.value = null
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Exception) {
                _knockAsk.value = foldContactAskFailed(_knockAsk.value, peer)
                errorMessage.value = e.message ?: e.toString()
            }
        }
    }

    /**
     * Follow = subscribe to the free "followers" tier (OTHER only; `profile.md`
     * § Where logic lives → *Follow / unfollow*). On success the button flips to
     * "Following". Mirrors linux `mod.rs::follow`.
     */
    fun follow() {
        val hex = targetActorId ?: return
        viewModelScope.launch {
            errorMessage.value = null
            _followWorking.value = true
            try {
                api.subscriptionSubscribe(hex, ProfileOffersVM.FOLLOWERS_TIER)
                _following.value = true
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _followWorking.value = false
            }
        }
    }

    /**
     * Read the viewed actor's contact edge (`fauna.contacts.list`) and set the
     * toggle's initial state: a `blocked` row for this peer → the button reads
     * "Unblock", any other/absent edge → "Block". Called on mount for an OTHER
     * profile. Mirrors linux `mod.rs::refresh_block_state`.
     */
    fun refreshBlockState() {
        val hex = targetActorId ?: return
        viewModelScope.launch {
            _blocked.value = runCatching {
                // Shared predicate (`contact_row_blocks_actor`): a `blocked` row whose
                // peer-id matches this actor (case-insensitive hex). Single-sourced so
                // the toggle state can't drift per-app. contacts.md § Where logic lives.
                api.fetchContacts(hex).any { com.fauna.ffi.contactRowBlocksActor(it.peerId, it.status, hex) }
            }.getOrDefault(false)
        }
    }

    /**
     * Block ⇄ unblock toggle (OTHER only; `profile.md` § User actions,
     * `contacts.md` § Where logic lives → Unblock). A not-blocked edge calls the
     * shared `knocks_block` over `fauna.knocks.block` (upsert → `blocked`); a
     * blocked edge calls `knocks_unblock` over `fauna.knocks.unblock` (the guarded
     * clear-the-edge, `ContactStatus` → `None`). The toggle is disabled during the
     * round-trip and flips state only on success. Mirrors linux `mod.rs::toggle_block`.
     */
    fun toggleBlock() {
        val hex = targetActorId ?: return
        val wasBlocked = _blocked.value
        viewModelScope.launch {
            errorMessage.value = null
            _blockWorking.value = true
            try {
                if (wasBlocked) {
                    api.unblockKnock(hex, hex)
                } else {
                    api.blockKnock(hex, hex)
                }
                _blocked.value = !wasBlocked
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _blockWorking.value = false
            }
        }
    }
}
