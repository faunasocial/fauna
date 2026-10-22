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
import com.fauna.app.ui.navigation.decodeProfileHome
import com.fauna.ffi.accountDisplayHandle
import com.fauna.ffi.actorHome
import com.fauna.ffi.actorIdFromSecret
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

    /**
     * Header fallback when no profile is published: SELF → the handle on the
     * CURRENT primary (`<handle>@<domain>`, the shared `accountDisplayHandle`,
     * never the bare handle — `mail-primary-domain-rename.md` § Display-handle
     * interaction) → own actor_id; OTHER → target actor_id. A function, not a
     * `val`: read from the live registry material at every [refreshHeader], so
     * a primary rename lands without rebuilding this VM.
     */
    private fun fallbackName(): String =
        if (isSelf) {
            selfDisplayHandle(secureStorage.handle, secureStorage.domain) ?: ownActorIdHex ?: ""
        } else {
            targetActorId ?: ""
        }

    private val _displayName = MutableStateFlow(fallbackName())

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

    /**
     * One header picture (`profile-avatar` / `profile-banner`): the hex blob hash
     * the profile names, and its fetched bytes — `null` while loading or after a
     * failed fetch, when the element paints empty (`profile.md` § Layout & flow).
     */
    class HeaderPicture(val hashHex: String, val bytes: ByteArray? = null)

    private val _avatar = MutableStateFlow<HeaderPicture?>(null)

    /** The stored avatar — `null` when the profile names none (no element). */
    val avatar: StateFlow<HeaderPicture?> = _avatar.asStateFlow()

    private val _banner = MutableStateFlow<HeaderPicture?>(null)

    /** The stored banner — `null` when the profile names none (no element). */
    val banner: StateFlow<HeaderPicture?> = _banner.asStateFlow()

    /** Bumped by every header read, so a picture an older read fetched (a save
     *  refetches both) never paints over a newer profile's. */
    private var pictureEpoch = 0

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

    /** False until the open-time edge read (or the viewer's own press) sets
     *  [blocked]: the `profile-block-button` is disabled meanwhile, since a press on
     *  the not-blocked default would re-send `knocks_block` for an actor the viewer
     *  already blocks (mirrors tui's `is_blocked: Option<bool>`). */
    private val _blockKnown = MutableStateFlow(false)
    val blockKnown: StateFlow<Boolean> = _blockKnown.asStateFlow()

    /** The viewer's own press has decided [blocked] for this open: a late
     *  open-time read must not rewind it. */
    private var blockUserAuthored = false

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
     * The home nest the open target carried (a Find User result's lookup, a post
     * author's `home_nest_url`, a contact row's, or the state-protocol entry's),
     * decoded off the route's `home` arg — `null` = none carried.
     */
    private val carriedHome: String? = decodeProfileHome(savedStateHandle.get<String>("home"))

    private val _actorHomeSlot = MutableStateFlow<String?>(null)

    /**
     * The page's **actor-home slot** (OTHER only; `profile.md` § Where logic lives →
     * *The actor's home*): the other actor's home nest, `null` = this nest. The
     * follow, the knock and the remote header read all read this one slot. Seeded at
     * open from the carried home alone, replaced after the header read by the shared
     * `actorHome` over the profile it fetched — never a per-gesture re-derivation.
     */
    val actorHomeSlot: StateFlow<String?> = _actorHomeSlot.asStateFlow()

    /** The shared `actorHome(carried, actor, body, own)` rule for this page's actor. */
    private fun actorHomeOver(body: ByteArray?): String? =
        targetActorId?.let {
            runCatching { actorHome(carriedHome, HexUtil.hexToBytes(it), body, api.nodeUrl) }.getOrNull()
        }

    /** What this VM last published to [ProfileTestState]; cleared only while it is still the published one. */
    private var publishedView: ProfileTestState.Viewed? = null

    private fun setActorHomeSlot(home: String?) {
        _actorHomeSlot.value = home
        publishedView = profileActorIdHex?.let { ProfileTestState.Viewed(it, home) }
        ProfileTestState.viewed = publishedView
    }

    init {
        setActorHomeSlot(actorHomeOver(null))
    }

    override fun onCleared() {
        if (ProfileTestState.viewed === publishedView) ProfileTestState.viewed = null
    }

    /**
     * Re-read the viewed actor's published profile (the shared
     * `profile_header_read`) and render its `display_name` into the header,
     * falling back to the handle/actor_id on `not_found`/error. Called on mount
     * and after a successful SELF edit-form publish. Mirrors linux
     * `mod.rs::refresh_header_name` (works for any actor).
     *
     * OTHER: the read goes to the actor-home slot as the open seeded it (this
     * nest first, then the relay to that home), and the profile it returns
     * replaces the slot through the shared `actorHome` rule — the profile the
     * header renders is the one whose home nest a follow or a knock from this
     * page goes to.
     *
     * The pictures the profile names ([avatar], [banner]) render at once, empty,
     * and paint as their bytes arrive from the nest that served the profile
     * (`relayedFrom`, pinned to the stamped `homeNestActorId`; `profile.md`
     * § Where logic lives → *Bytes direct, as for a relayed post*) — tui's
     * `fetch_pictures`. Every read refetches both.
     */
    fun refreshHeader() {
        val hex = profileActorIdHex ?: return
        viewModelScope.launch {
            val read = api.profileHeaderRead(hex, if (targetActorId != null) _actorHomeSlot.value else null)
            val body = read?.body
            val display = body?.let { runCatching { api.decodeProfileDisplay(it) }.getOrNull() }
            val published = display?.displayName?.takeIf { it.isNotBlank() }
            _publishedName.value = published
            _displayName.value = published ?: fallbackName()
            if (targetActorId != null) setActorHomeSlot(actorHomeOver(body))
            val epoch = ++pictureEpoch
            for ((flow, hash) in listOf(_avatar to display?.avatarHashHex, _banner to display?.bannerHashHex)) {
                flow.value = hash?.let { HeaderPicture(it) }
                if (hash == null) continue
                launch {
                    val bytes = api.fetchProfilePicture(read?.relayedFrom, read?.homeNestActorId, hash)
                    if (bytes != null && epoch == pictureEpoch) flow.value = HeaderPicture(hash, bytes)
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
     * page's actor-home slot holds ([actorHomeSlot]). Reads "Request sent" and stays
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
        val route = _actorHomeSlot.value
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
     *
     * The follow carries the page's actor-home slot: an author homed on another
     * nest is followed across nests (`federated-follow.md` § Decision 2), and a
     * `null` slot is today's local follow. Mirrors tui's follow.
     */
    fun follow() {
        val hex = targetActorId ?: return
        val home = _actorHomeSlot.value
        viewModelScope.launch {
            errorMessage.value = null
            _followWorking.value = true
            try {
                api.subscriptionSubscribe(hex, ProfileOffersVM.FOLLOWERS_TIER, home)
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
            applyBlockRead(runCatching {
                // Shared predicate (`contact_row_blocks_actor`): a `blocked` row whose
                // peer-id matches this actor (case-insensitive hex). Single-sourced so
                // the toggle state can't drift per-app. contacts.md § Where logic lives.
                api.fetchContacts(hex).any { com.fauna.ffi.contactRowBlocksActor(it.peerId, it.status, hex) }
            }.getOrDefault(false))
        }
    }

    /**
     * Fold the open-time edge read in: it enables the toggle (a failed read lands as
     * not-blocked, so the toggle never stays stuck off) and sets [blocked] — unless
     * the viewer's own press already authored the edge, which a read issued before
     * it must not rewind. Mirrors linux `apply_block_read`.
     */
    internal fun applyBlockRead(isBlocked: Boolean) {
        if (!blockUserAuthored) _blocked.value = isBlocked
        _blockKnown.value = true
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
        if (!_blockKnown.value) return
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
                blockUserAuthored = true
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _blockWorking.value = false
            }
        }
    }
}

/**
 * The open profile's viewed actor and actor-home slot, published for the e2e
 * agent's `data.profile` state (`actor_id`, `home_nest_url` — the keys every
 * app answers from the page it painted; `actions/profile.py::wait_for_viewed_actor`
 * / `wait_for_home_nest_url`). Null while no profile page is open.
 */
object ProfileTestState {
    class Viewed(val actorId: String, val homeNestUrl: String?)

    @Volatile
    var viewed: Viewed? = null
}

/**
 * The profile SELF header's display handle from the account's cached
 * handle/domain — the shared `fauna_client_accounts::display_handle` (one
 * resolution for all 7 apps); `null` while nothing resolves.
 */
internal fun selfDisplayHandle(handle: String?, domain: String?): String? =
    accountDisplayHandle(handle.orEmpty(), handle, domain)
