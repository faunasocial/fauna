package com.fauna.app.p2pshare

import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility

/**
 * App-owned mirrors of the co-present offline-share ceremony's read models —
 * the `p2p-share` member's ceremony half (`docs/goal/behavior/p2p.md`
 * § Offline share initiation) — and the reason they exist at all
 * (`dynamic-features.md` § Platform-family surface excision).
 *
 * The payments plane's seam (`com.fauna.app.payments.PaymentsRows`), one
 * member over. Kotlin has no inline compile-time exclusion, and a store-safe
 * build links a `fauna-ffi` with `offline-share` excised: its generated face
 * has no `FfiCeremonySeat`, no `FfiGroupShareViews` and none of
 * `fauna-client-capabilities`' `OfflineSharePanel` / `CeremonyStatus` /
 * `OfflineShareView`. So everything that names one lives in the
 * `src/p2pShare` / `src/noP2pShare` twins of [OfflineShareHost], and the
 * shared view model and Composables are typed on *these*.
 */

/** Which panel of the affordance is open. Mirrors `OfflineSharePanel`. */
enum class OfflinePanel { CLOSED, INITIATE, RECEIVE }

/**
 * The affordance's whole paint decision, from shared Rust: its
 * `OfflineShareView`, that view's `offline_share_gates`, the begin / expect
 * buttons' shared answers, and the two readings
 * (status, typed-code refusal) still as [LocalizedText] — the Screen resolves
 * them, so this record carries no Android context.
 */
data class OfflineShareDecision(
    val panel: OfflinePanel,
    /** This device's compare code — the actor key, addressed once the seat binds. */
    val ownCode: String,
    /** The typed counterpart code, verbatim. */
    val peerCode: String,
    val statusLabel: LocalizedText,
    /** Why the typed code is refused — `null` for a usable or not-yet-typed one. */
    val codeError: LocalizedText?,
    val showsEntryButtons: Boolean,
    val showsCodeWidgets: Boolean,
    /** `offline-share-begin-button`'s answer (`offline_share_begin_possibility`). */
    val beginAnswer: Possibility,
    /** `offline-receive-expect-button`'s answer (`offline_share_expect_possibility`). */
    val expectAnswer: Possibility,
    val showsCancel: Boolean,
)

/** One offered set awaiting this account's consent. Mirrors `FfiPendingGroupShare`. */
data class GroupInvitation(
    /** The accept/decline target — by id, never by row position. */
    val scopeId: ByteArray,
    /** Who offered it, as a short actor id. */
    val initiator: String,
    /** The nameless set's short scope id. */
    val shortId: String,
) {
    // ByteArray compares by identity; a data class over one needs these by hand.
    override fun equals(other: Any?): Boolean =
        other is GroupInvitation && scopeId.contentEquals(other.scopeId) &&
            initiator == other.initiator && shortId == other.shortId

    override fun hashCode(): Int = (scopeId.contentHashCode() * 31 + initiator.hashCode()) * 31 + shortId.hashCode()
}

/** One shared set this device holds the machinery for. Mirrors `FfiGroupScope`. */
data class GroupScope(
    val shortId: String,
    val memberCount: UInt,
    /** Who minted it — `null` on the initiator's own set. */
    val sharedBy: String?,
    /**
     * The set's `FolderRef::Group` wire form (`group:<64-hex>`,
     * `FfiGroupScope.folder_id`) — the ref its `folder-on-demand-toggle`
     * preference and its documents-provider presence are keyed by
     * (`group-set.md` § Build design — the group set, decision 11 (a)).
     */
    val folderId: String,
)

/** Both halves of the Folders page's group surface, from one read. Mirrors `FfiGroupShareViews`. */
data class GroupShareRows(
    val invitations: List<GroupInvitation> = emptyList(),
    val scopes: List<GroupScope> = emptyList(),
)
