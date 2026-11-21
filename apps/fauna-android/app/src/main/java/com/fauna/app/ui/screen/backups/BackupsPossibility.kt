package com.fauna.app.ui.screen.backups

import com.fauna.ffi.FfiDestinationForm

// The Backups page's "offer only what is possible" seam on android
// (`docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4, 6, 7;
// `docs/goal/ui/backups.md` § Errors & edge cases → *What the page offers*).
// Every answer is the shared `fauna_backups_machine::possibility` predicate over
// UniFFI; this file only maps the screen's own state onto the predicate's
// inputs, so the mapping itself is provable without a composable.

/**
 * Which ceremony the destination dialog runs — what decides which of its fields
 * the shared `destination_form_possibility` must see. [custodian] is the
 * dialog's kind already resolved against the shared kind catalog (on edit, the
 * row's own kind), never a string match here.
 */
internal fun destinationFormOf(editing: Boolean, custodian: Boolean): FfiDestinationForm = when {
    editing && custodian -> FfiDestinationForm.EDIT_CLIENT_DEVICE
    editing -> FfiDestinationForm.EDIT_NEST
    custodian -> FfiDestinationForm.ADD_CLIENT_DEVICE
    else -> FfiDestinationForm.ADD_NEST
}

/**
 * The restore section's selection as the shared `restore_possibility` takes it:
 * the picked snapshot's id (`null` = there is none to restore) and whether its
 * kind's `restore-kind-checkbox` is ticked (a kind with no box is not the
 * checkboxes' to refuse: `true`).
 */
internal data class RestoreAsk(val snapshotId: String?, val kindTicked: Boolean)

internal fun restoreAskOf(
    selectedId: Long?,
    selectedKind: String?,
    kinds: List<String>,
    kindChecked: List<Boolean>,
): RestoreAsk {
    val index = kinds.indexOf(selectedKind)
    return RestoreAsk(
        snapshotId = selectedId?.toString(),
        kindTicked = index < 0 || kindChecked.getOrElse(index) { true },
    )
}
