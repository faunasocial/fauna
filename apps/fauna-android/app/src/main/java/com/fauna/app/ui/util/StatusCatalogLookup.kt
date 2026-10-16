package com.fauna.app.ui.util

import android.content.Context
import com.fauna.ffi.FfiStatusLookup

/**
 * The app's own string catalog handed to the shared `statusText` projection
 * (`ui/status.md` § State & data shape). Returns the localized template for
 * `key` — its `{name}` tokens intact, the shared projection fills them — or
 * null when the catalog has no such key (the projection then falls back to the
 * key itself, so a miss surfaces rather than rendering blank). Same resource
 * mapping as [resolveLocalized] (`status.sync.pending_summary` →
 * `status_sync_pending_summary`).
 */
class StatusCatalogLookup(private val context: Context) : FfiStatusLookup {
    override fun lookup(key: String): String? {
        val resId = context.resources.getIdentifier(key.replace('.', '_'), "string", context.packageName)
        return if (resId == 0) null else context.getString(resId)
    }
}
