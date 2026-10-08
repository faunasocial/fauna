package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.ReportSheetStore
import com.fauna.ffi.FfiReportForm
import com.fauna.ffi.FfiReportTarget
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The thin handle every surface uses to reach the ONE report sheet
 * (`moderation.md` § User-initiated reporting → *App surface*). The state itself
 * is [ReportSheetStore], app-scoped inside [ContentPolicyStore] — a verb on the
 * feed, a conversation bubble or a profile only calls [open], and the
 * [com.fauna.app.ui.components.ReportHost] mounted once in the shell paints
 * whatever is open and calls [submit]. Different `ReportVM` instances on
 * different screens therefore share one sheet.
 */
@HiltViewModel
class ReportVM @Inject constructor(
    private val contentPolicy: ContentPolicyStore,
) : ViewModel() {
    val state: StateFlow<ReportSheetStore.State> = contentPolicy.report.state

    fun open(target: FfiReportTarget) = contentPolicy.report.open(target)

    fun cancel() = contentPolicy.report.cancel()

    fun edit(transform: (FfiReportForm) -> FfiReportForm) = contentPolicy.report.edit(transform)

    fun submit() {
        viewModelScope.launch { contentPolicy.report.submit() }
    }
}
