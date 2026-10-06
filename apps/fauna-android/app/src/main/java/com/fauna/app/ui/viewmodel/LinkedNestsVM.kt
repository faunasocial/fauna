package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.classifyLinkInput
import dagger.hilt.android.lifecycle.HiltViewModel
import uniffi.fauna_client_pair.LinkInput
import uniffi.fauna_client_pair.LinkedNestStatus
import uniffi.fauna_client_pair.LinkedNestsAction
import uniffi.fauna_client_pair.LinkedNestsMachine
import uniffi.fauna_client_pair.LinkedNestsSnapshot
import uniffi.fauna_client_pair.TrustGrantDuration
import uniffi.fauna_client_pair.TrustLens
import uniffi.fauna_client_pair.TrustScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Renders the shared `LinkedNestsMachine` (libs/fauna-client-pair, over UniFFI)
 * for the user-settings Nests page. Per priority #2 this view-model holds
 * **no** pairing/trust logic — it owns the machine, mirrors its snapshot into a
 * StateFlow the Compose screen collects, and dispatches actions. All sequencing
 * (parse the Ed25519 identity, default-capability selection, the list/add/revoke
 * round-trips, the holder discovery + grant-log folds behind the trust facet,
 * the admin-policy error) lives in the shared machine; the other five apps
 * render the identical snapshot.
 *
 * Target state: docs/goal/ui/nests.md (page UX + trust facet) +
 * docs/goal/behavior/linked-nests.md (the linking half, unchanged). The Linux
 * lead is apps/fauna-linux/src/settings/linked_nests.rs (same machine, same
 * flows). This VM dispatches Link/Unlink/SetLens/Renew/Revoke/Mint — the
 * mint flow (`nests.md` § Mint) landed after the audit-only v1 shell.
 */
@HiltViewModel
class LinkedNestsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    // Null until the WS-RPC connection is up; the page stays empty until then.
    private val machine: LinkedNestsMachine? = api.buildLinkedNestsMachine()

    /** The rendered snapshot. The screen collects this and re-reads on change. */
    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)

    /** Last action's error (the admin-policy rejection lands here too). */
    val errorMessage = MutableStateFlow<String?>(null)

    /**
     * The custodian-NEST rows (`nests.md` § Trust facet — custody rows): the
     * NEST-anchored half of the SAME custody fold the Devices page paints (its
     * complement — one custody never renders on both pages). Kept across an
     * unreadable pass: a null fold is a transient, never "no custodians".
     */
    val custodyNestRows =
        MutableStateFlow<List<uniffi.fauna_client_capabilities.CustodyHolderRowView>>(emptyList())

    /** Hex identities holding this account's escrow — the badge source. */
    val escrowHolders = MutableStateFlow<Set<String>>(emptySet())

    init {
        hydrate()
        loadCustodyAndEscrow()
        // The store-change notice: the custody fold and the escrow holders are
        // read through the account store, so the open page re-reads both
        // ([ApiClient.storeChangedTick]).
        viewModelScope.launch {
            api.storeChangedTick.collect {
                hydrate()
                loadCustodyAndEscrow()
            }
        }
    }

    /**
     * List the owner's pairings (`fauna.pair.list`) and render. The WS socket
     * comes up shortly after login but the screen can mount first, so the first
     * call may fail with a disconnect — retry a few times (mirrors the Linux
     * lead's hydrate-retry in settings/linked_nests.rs).
     */
    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
            }
            publish(m)
        }
    }

    /**
     * Dispatch the link action the entered value resolves to (`fauna.pair.add`).
     * The value is routed through the shared `classifyLinkInput` so every app
     * resolves the same input identically (priority #2): a nest **address** (URL)
     * seeds the authorization row on BOTH ends in one action (`LinkBoth` — the
     * common "log into 2 nests, then pair them" case), while a bare 64-hex Ed25519
     * **identity** authorizes that one nest (`Link`, the out-of-band single-end
     * path). Empty/blank input is ignored; empty capabilities → the machine's
     * canonical full self-sync set. Mirrors the linux lead's `submit_link`
     * (apps/fauna-linux/src/settings/linked_nests.rs).
     */
    fun link(raw: String) {
        val value = raw.trim()
        if (value.isEmpty()) return
        val action = when (val classified = classifyLinkInput(value)) {
            is LinkInput.NestUrl -> LinkedNestsAction.LinkBoth(
                otherNestUrl = classified.nestUrl,
                capabilities = emptyList(),
                expiresAt = null,
                label = null,
            )
            is LinkInput.NestId -> LinkedNestsAction.Link(
                nestId = classified.nestId,
                capabilities = emptyList(),
                expiresAt = null,
                label = null,
                nestUrl = null,
            )
        }
        dispatch(action)
    }

    /**
     * Load the custody fold's nest-anchored rows and the escrow holders, on the
     * page's own hydrate edge (tui's Nests hydrate reads both beside the
     * machine). Failures keep what is painted.
     */
    private fun loadCustodyAndEscrow() {
        viewModelScope.launch {
            runCatching { api.custodyFacetLoad() }.getOrNull()?.let { facet ->
                custodyNestRows.value = facet.rows.filter { it.custodianNestUrl != null }
            }
            runCatching { api.custodyEscrowHolders() }.getOrNull()?.let {
                escrowHolders.value = it.toSet()
            }
        }
    }

    /**
     * Revoke a custodian nest's custody (`nest-trust-custody-revoke-button`) —
     * the same shared act the Devices page's custody revoke runs (the nest's
     * revoke BEFORE the signed record lives in shared Rust). Carries the grant
     * id + accept-bound custodian key, never a row index; the error reaches the
     * page banner (convention 11) and the refold drops the revoked row.
     */
    fun revokeCustody(grantId: ByteArray, holder: ByteArray?) {
        viewModelScope.launch {
            val outcome = runCatching { api.custodyRevoke(grantId, holder) }.getOrElse { e ->
                errorMessage.value = e.message
                return@launch
            }
            outcome.facet?.let { facet ->
                custodyNestRows.value = facet.rows.filter { it.custodianNestUrl != null }
            }
            outcome.error?.let { errorMessage.value = it }
        }
    }

    /** Unlink a nest (`fauna.pair.revoke`); the machine re-lists on success. */
    fun unlink(nestId: String) = dispatch(LinkedNestsAction.Unlink(nestId = nestId))

    /** Flip a nest row's trust-facet lens (Now ⇄ History) — local UI state, no
     *  nest round-trip (`nests.md` § Trust facet). */
    fun setLens(nestId: String, lens: TrustLens) =
        dispatch(LinkedNestsAction.SetLens(nestId = nestId, lens = lens))

    /**
     * Mint a scope-first trust grant to one of the nest's content-processor
     * holders (`mint_grant` + `fauna.capabilities.mint` + a `Mint` event). The
     * shell derives `holderBridgeId` from the scope choice (`nests.md` § Mint);
     * `scope` is the chosen option's content (e.g. `content.read{post, tier}`).
     * `duration` is the owner's pick in `nest-trust-mint-duration-select`
     * (the row's `mintDefaultDuration` until they choose — `nests.md` § Expiry
     * / renewal → *Duration and blessing*).
     */
    fun mint(
        nestId: String,
        holderBridgeId: String,
        scope: List<TrustScope>,
        duration: TrustGrantDuration,
    ) =
        dispatch(
            LinkedNestsAction.Mint(
                nestId = nestId,
                holderBridgeId = holderBridgeId,
                scope = scope,
                duration = duration,
            ),
        )

    /** Bless (or un-bless) a nest (`nest-trust-blessed-toggle`) — a
     *  `fauna.state.blessed-nests` write through the shared machine; blessed standing
     *  grants then renew themselves. */
    fun setBlessed(nestId: String, blessed: Boolean) =
        dispatch(LinkedNestsAction.SetBlessed(nestId = nestId, blessed = blessed))

    /** Renew a grant's window (`fauna.capabilities.renew` + a `Renew` event). */
    fun renew(grantId: ByteArray) = dispatch(LinkedNestsAction.Renew(grantId = grantId))

    /** Revoke a grant (`fauna.capabilities.revoke` + a `Revoke` event); the
     *  holder's next fetch goes dark. */
    fun revoke(grantId: ByteArray) = dispatch(LinkedNestsAction.Revoke(grantId = grantId))

    /**
     * Freeze the home nest's `NestBackupKey` seal grant (`fauna.backup.nest_key.revoke`,
     * spoken to the source nest): it can seal and upload no NEW message backups.
     * Custody a destination already holds is untouched (`nests.md` § Trust facet —
     * backup rows).
     */
    fun revokeBackupSeal() = dispatch(LinkedNestsAction.RevokeBackupSeal)

    /**
     * Re-arm the user's own queued forwards for an immediate retry
     * (`fauna.pair.forward_retry`), then re-list (`nests.md` § Forward queue).
     */
    fun retryForwards() = dispatch(LinkedNestsAction.RetryForwards)

    /**
     * Stop forwarding the user's queued posts to their relay
     * (`fauna.pair.forward_discard`), then re-list; the posts themselves stay.
     */
    fun discardForwards() = dispatch(LinkedNestsAction.DiscardForwards)

    /**
     * Withdraw the home nest's authorization to write custody at ONE destination
     * (`fauna.backup.writer_grant.revoke`). The shared machine speaks this over
     * **the destination's own** authenticated connection, at the URL from the
     * client's own pinned config — never routed through the source nest, which is
     * what keeps the affordance operable with that nest fully hostile.
     */
    fun revokeBackupWriter(destinationId: String) =
        dispatch(LinkedNestsAction.RevokeBackupWriter(destinationId = destinationId))

    /**
     * Roll one retained backup generation back to live
     * (`fauna.backup.generation.restore`, spoken to the destination on the
     * client's own authenticated connection — never through the source nest,
     * which is what keeps recovery operable with that nest fully hostile).
     * The row's own address triple round-trips unchanged (`nests.md` §
     * Trust facet — generation recovery); never a row index, since the
     * flattened `trustGenerations` list spans destinations.
     */
    fun restoreGeneration(destinationId: String, folderName: String, pathHash: String, manifestHash: String) =
        dispatch(
            LinkedNestsAction.RestoreGeneration(
                destinationId = destinationId,
                folderName = folderName,
                pathHash = pathHash,
                manifestHash = manifestHash,
            ),
        )

    private fun dispatch(action: LinkedNestsAction) {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            publish(m)
        }
    }

    private fun publish(m: LinkedNestsMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        val EMPTY_SNAPSHOT = LinkedNestsSnapshot(
            // `null` = the pairing-only default (no trust facet wired) — the
            // documented empty-state value, not a stub (call-site update for the
            // additive `LinkedNestsSnapshot.home` field).
            home = null,
            pairings = emptyList(),
            status = LinkedNestStatus.IDLE,
            error = null,
            // No restore has run on an empty snapshot (call-site update for the
            // additive `LinkedNestsSnapshot.restoreOutcome` field).
            restoreOutcome = null,
        )
    }
}
