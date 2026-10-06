package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.ffi.FfiContactOverlays
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import javax.inject.Inject

/**
 * What the viewer calls a person, for the surfaces keyed on that person — the
 * roster row, the knock sender, the feed and subscription author, the Profile
 * header (`contacts.md` § The private overlay → *Where the nickname paints*).
 *
 * Pure pass-through to the shared projection (`FfiContactOverlays`): a screen
 * takes this beside its own view-model, collects [epoch], and keys every
 * [overlays] read on it — no screen resolves a name, joins a label line or
 * holds the face. Member chips and message senders never read here; their
 * names come from the conversations snapshot, which applies the paint gate.
 */
@HiltViewModel
class ContactOverlaysVM @Inject constructor(
    private val host: ConversationsManagerHost,
) : ViewModel() {

    /** Moves whenever a name read through [overlays] may have changed. */
    val epoch: StateFlow<Long> = host.overlayEpoch

    /** The projection of the live manager — read it afresh each time [epoch] moves. */
    fun overlays(): FfiContactOverlays = host.contactOverlays()
}
