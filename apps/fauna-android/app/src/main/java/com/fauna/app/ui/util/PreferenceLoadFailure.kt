package com.fauna.app.ui.util

import com.fauna.ffi.FfiException
import com.fauna.ffi.FfiTrainedTopicsException
import com.fauna.ffi.refusedAsNotReady

/**
 * The text of a failed preference-page load, unwrapped from the generated
 * exception: a `General` variant's `msg` is the user-facing line, while the
 * generated `message` is the aggregated `"msg=…"` form (the same unwrap
 * `AccountSettingsVM.switcherErrorLine` makes), so the field is read, never
 * the `message` painted.
 */
fun Exception.loadFailureText(): String? = when (this) {
    is FfiException.General -> msg
    is FfiTrainedTopicsException.General -> msg
    else -> message
}

/**
 * The failure text when it is the first-listing gate's refusal
 * (`common.needs_nest` / `common.needs_other_device`), else `null` — the one
 * shared predicate [refusedAsNotReady] over [loadFailureText], so a page whose
 * background load is otherwise retried quietly says why it has not loaded
 * (`account-client-lifecycle.md` § The client-side lifecycle → *The first
 * listing*, clauses (4) and (5)).
 */
fun Exception.firstListingRefusal(): String? =
    loadFailureText()?.takeIf(::refusedAsNotReady)
