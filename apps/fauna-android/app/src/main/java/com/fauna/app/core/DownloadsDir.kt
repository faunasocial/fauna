package com.fauna.app.core

import android.content.Context
import java.io.File

/**
 * The app's download directory: the app cache's `fauna/` directory — the
 * `cache-path` root `res/xml/provider_paths.xml` exposes, so any file in it can
 * be offered to the share sheet. ONE directory for every android download
 * surface (`ui/util/ShareFile.kt`'s files, the mailbox export archive the
 * shared sink streams straight to disk), which is also what makes a download
 * observable to the e2e harness (the bridge's `/download-dir` route reads it).
 * Created on use.
 */
fun Context.downloadsDir(): File = File(cacheDir, "fauna").apply { mkdirs() }
