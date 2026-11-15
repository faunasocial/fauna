package com.fauna.app.ui.viewmodel

import com.fauna.app.p2pshare.SharePlaneReadings
import com.fauna.app.p2pshare.SharePlaneReadingsSource
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** A plane that is not running on this device — the surface's `null` reading. */
object NoSharePlaneReadings : SharePlaneReadingsSource {
    override val readings: StateFlow<SharePlaneReadings?> = MutableStateFlow(null)
}
