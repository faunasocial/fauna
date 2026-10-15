package com.fauna.app.core

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The open inherited-rule marks (`succession-aftermath.md` § Adjudicating what
 * the aftermath carries across, the email-filter plane) — the ids of every
 * filter rule the aftermath carried across that the owner has not yet
 * answered, read through the fauna-ffi façade (`filter_marks_list`).
 *
 * ONE cache feeding both surfaces, apple's `FaunaClient.inheritedFilterMarks`
 * shape: Settings ▸ Privacy paints `filter-unattested-mark` +
 * `filter-review-keep-button` off it, and its count is Settings ▸ Account's
 * `recovery-kit-inherited-filters-status` — so a Keep or a delete takes the
 * line down with the mark. `@Singleton` (not a ViewModel) so the two pages'
 * VMs cannot drift apart.
 *
 * Re-read on every Account entry, every Privacy visit, after every verdict and
 * — the fourth trigger the other apps have — at the aftermath sink's
 * `config_stage_settled` hook, fired once the shared driver's raises have
 * written ([SuccessionAftermathHost.onConfigStageSettled], behind the post-auth
 * `run_succession_aftermath` call).
 *
 * **A failed read keeps the previous marks** rather than blanking them: a
 * transport blip must never silently hide a mark the owner has not answered
 * (linux `read_filter_marks`' "leave the cache alone" contract). That makes
 * the identity reset load-bearing — the marks are one account's ledger, so an
 * account switch or sign-out drops them (`AccountStores` closer), and a failed
 * first read for the next account cannot paint the previous one's count.
 */
@Singleton
class InheritedFilterMarks @Inject constructor(
    private val api: ApiClient,
    accountStores: AccountStores,
) {
    private val _marks = MutableStateFlow<List<Long>>(emptyList())
    val marks: StateFlow<List<Long>> = _marks.asStateFlow()

    init {
        accountStores.registerCloser("inherited-filter-marks") { _marks.value = emptyList() }
    }

    /** Re-read the open marks. Best-effort: a failure keeps the previous ones. */
    suspend fun reload() {
        try {
            _marks.value = api.filterMarksList()
        } catch (e: Exception) {
            ShellLog.w("InheritedFilterMarks", "re-reading the inherited-filter marks failed: ${e.message}")
        }
    }

    /**
     * `filter-review-keep-button` — the owner recognises this rule: it stays,
     * and its mark clears. Throws on failure, for the caller to surface: a Keep
     * that silently did not land leaves the owner believing they answered a
     * question that will be asked again. Re-reads either way.
     */
    suspend fun keep(filterId: Long) {
        try {
            api.filterMarkKeep(filterId)
        } finally {
            reload()
        }
    }

    /**
     * Record **Removed** for a rule whose `filter-delete` has ALREADY landed —
     * that delete IS the Remove half (no second removal mechanism), so the
     * verdict is recorded strictly after it: a failure leaves a re-asked
     * question about a rule that is gone, never a silenced armed rule.
     * Best-effort for the same reason; a no-op for an unmarked row.
     */
    suspend fun recordRemoved(filterId: Long) {
        if (filterId !in _marks.value) return
        try {
            api.filterMarkRemoved(filterId)
        } catch (e: Exception) {
            ShellLog.w("InheritedFilterMarks", "recording the inherited-rule removal failed: ${e.message}")
        }
        reload()
    }
}
