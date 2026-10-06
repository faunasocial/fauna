package com.fauna.app.ui.util

import android.content.Context
import android.content.Intent
import androidx.core.content.FileProvider
import com.fauna.app.core.downloadsDir
import java.io.File

/**
 * Android's save path for bytes the app produced (a backups single-file restore,
 * the data export, a Media download): write them to a file under
 * [downloadsDir] and hand that
 * file to the system share sheet (`FileProvider` + `ACTION_SEND`), from which the
 * user saves it to Files, Drive, or any other target. One helper, so every
 * download surface saves the same way.
 *
 * The file is named after [fileName]'s last path component (never a separator,
 * so a name can not steer the write out of the cache directory; blank →
 * `download`), which is also the chooser's title unless [chooserTitle] is given.
 * Throws on an I/O or intent failure — the caller surfaces it.
 */
fun Context.shareFileBytes(
    fileName: String,
    bytes: ByteArray,
    mimeType: String = "application/octet-stream",
    chooserTitle: String? = null,
) {
    val name = fileName.substringAfterLast('/').ifBlank { "download" }
    val out = File(downloadsDir(), name)
    out.writeBytes(bytes)
    shareFile(out, mimeType, chooserTitle)
}

/**
 * Hand a file the app already holds on disk to the system share sheet — the
 * streaming twin of [shareFileBytes] for something too large to pass through
 * memory (the mailbox export archive, up to 10 GiB). [file] must sit under a
 * root `res/xml/provider_paths.xml` exposes. Throws on an intent failure — the
 * caller surfaces it.
 */
fun Context.shareFile(
    file: File,
    mimeType: String = "application/octet-stream",
    chooserTitle: String? = null,
) {
    val uri = FileProvider.getUriForFile(this, "$packageName.fileprovider", file)
    val intent = Intent(Intent.ACTION_SEND).apply {
        type = mimeType
        putExtra(Intent.EXTRA_STREAM, uri)
        addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }
    startActivity(Intent.createChooser(intent, chooserTitle ?: file.name))
}
