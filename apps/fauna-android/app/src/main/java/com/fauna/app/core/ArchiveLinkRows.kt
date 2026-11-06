package com.fauna.app.core

import android.content.Context

/**
 * What one Contacts row paints for phase two of archive import
 * (`docs/goal/behavior/archive-import.md` § Linking and merging, ruling 8):
 * `contact-archive-link-prompt` with its yes / no while the pair is asked,
 * `contact-archive-linked` with unlink while linked, nothing in any other
 * state. Each carries the row's own contact and platform into the shared calls.
 *
 * Declared in `src/main` because the Contacts row (compiled into every build
 * type) paints it; the views it is made from are the machine crate's
 * `ArchiveLinkView`, which only the `src/noKids` half names — the kids
 * `fauna-ffi` flavor exports no archive import. The FaunaKit
 * `ArchiveLinkRowPaint` twin.
 */
sealed interface ArchiveLinkRowPaint {
    val contact: String
    val platform: String

    data class Asked(val prompt: String, override val contact: String, override val platform: String) :
        ArchiveLinkRowPaint

    data class Linked(val label: String, override val contact: String, override val platform: String) :
        ArchiveLinkRowPaint
}

/**
 * The paints for the roster row of `peerId`, whose shown name is
 * `contactName`, worded through [context] — one value per read of the shared
 * views, so a changed read is a new value the screen recomposes on.
 * [NONE] paints nothing: before the first read, for a login that wired no
 * links, and always in the kids build.
 */
fun interface ArchiveLinkPainter {
    fun paints(context: Context, peerId: String, contactName: String): List<ArchiveLinkRowPaint>

    companion object {
        val NONE = ArchiveLinkPainter { _, _, _ -> emptyList() }
    }
}
