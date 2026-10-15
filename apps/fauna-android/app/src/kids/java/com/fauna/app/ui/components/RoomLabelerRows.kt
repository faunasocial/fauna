package com.fauna.app.ui.components

import androidx.compose.runtime.Composable
import uniffi.fauna_conversations.RoomSettingsDraft

// The room editor's labeler rows — **the excised half**. Compiled into the
// `kids` build type only, in place of the `src/noKids/` twin, and empty by
// design: the community-labeler catalog is third-party anything, which a kids
// artifact does not contain (`family-safety.md` § The account age band;
// `dynamic-features.md` § Compile-time excision).

/** Excised — no community-labeler catalog in a kids build. */
@Suppress("UNUSED_PARAMETER")
@Composable
fun RoomLabelerRows(
    draft: RoomSettingsDraft,
    canSetPolicy: Boolean,
    onDraftChange: (RoomSettingsDraft) -> Unit,
) = Unit
