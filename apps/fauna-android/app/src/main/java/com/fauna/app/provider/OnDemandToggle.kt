package com.fauna.app.provider

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import uniffi.fauna_devices_machine.FolderSummary

/**
 * The Folders page's seam onto the on-demand presence
 * (`folder-on-demand-toggle`, "Show in Files" — `on-demand-files.md`
 * § Android SAF DocumentsProvider binding): the device-local preference the
 * toggle reads and writes, the stronger presence that replaces it, and the
 * re-plan that makes a flip take effect in the system file browser at once.
 * The production implementation is [FfiOnDemandSource], which owns the shared
 * preference store and the plan; the page never spells an identifier or reads
 * the store itself.
 */
interface OnDemandToggle {
    /**
     * The row's set identity: its `FolderRef` (the shared `folderRefForRow` —
     * an own row, a same-nest member row and a cross-nest member row alike) and
     * the actor-scoped identifier the preference store and the provider's
     * document ids share. `null` when signed out or the row resolves no ref:
     * such a row has no presence to steer, so it renders no toggle.
     */
    suspend fun identify(folder: FolderSummary): OnDemandIdentity?

    /**
     * Read every given set's state in one pass — the preference (default ON),
     * and whether the set is presented a stronger way on this device (a
     * watched-directory ingress feeds it) — keyed by scoped id.
     */
    suspend fun read(sets: List<OnDemandIdentity>): Map<String, OnDemandToggleState>

    /** Persist the preference only; [replan] applies it. */
    suspend fun setShown(scopedId: String, shown: Boolean)

    /**
     * Re-run the shared presence plan and tell the file browser its roots
     * changed, so a set appears in or leaves Files at once.
     */
    suspend fun replan()

    /**
     * Ticks after every presence reconcile the provider runs, whoever started
     * it (the picker's own refresh included), so the page re-reads each row's
     * park then — the app watches the park, it is never told (`file-sync.md`
     * § Multi-writer shared sets → *Revocation on a capability host*, "What
     * the app shows, and how the park reaches it").
     */
    val reconciled: Flow<Unit> get() = emptyFlow()
}

/**
 * A set's identity on this device's presence surface. [rowReadsReader] is the
 * page's own row reading — a member row whose access is not `writer` — which
 * the park fold takes over the last plan's: a demotion has no push, so the
 * folder-list refresh can read it before the provider's next reconcile does.
 */
data class OnDemandIdentity(
    val scopedId: String,
    val folderRef: String,
    val rowReadsReader: Boolean = false,
)

/** One row's `folder-on-demand-toggle` reading. */
data class OnDemandToggleState(
    /** The preference — default ON. */
    val shown: Boolean,
    /**
     * A watched-directory ingress feeds the set on this device: the explicit
     * act outranks the ambient default, so the row shows why instead of a
     * toggle (the apple bound-location caption's android twin).
     */
    val stronger: Boolean,
    /**
     * The set is parked on this device's documents provider: the host
     * persisted its revocation mark, or the row reads `reader` while the
     * provider still keeps bodies the nest never recorded — the shared
     * `on_demand_parked_presences` fold. It is `binding_section`'s `parked`
     * input; a set this device does not present reads `false`.
     */
    val parked: Boolean = false,
)
