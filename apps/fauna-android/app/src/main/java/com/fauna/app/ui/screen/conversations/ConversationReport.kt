package com.fauna.app.ui.screen.conversations

import com.fauna.app.core.HexUtil
import com.fauna.ffi.FfiReportTarget
import com.fauna.ffi.reportMessageTarget
import uniffi.fauna_conversations.MessageSnapshot
import uniffi.fauna_conversations.TypedAddress

/**
 * The report identity of a conversation message (`moderation.md` § User-initiated
 * reporting → *App surface*) — the android twin of apple's `DmMessageBubble`
 * `senderActorHex` / `reportTarget`.
 *
 * The sender's hex actor id for a Fauna-rail address, `null` for mail and bridged
 * senders: it routes a report to the author's home nest and keys the account-level
 * hide.
 */
internal fun messageSenderActorHex(msg: MessageSnapshot): String? =
    (msg.sender as? TypedAddress.Fauna)?.let { HexUtil.bytesToHex(it.actorId) }

/**
 * The report target for a RECEIVED conversation message off its plane ref
 * (`reportMessageTarget` — the shared parse carries the sealed rule), or `null`
 * for an own message or a mail/bridged one, which has no plane identity and paints
 * no report verb. Short-circuits before the FFI call so a bubble that cannot be
 * reported costs nothing.
 */
internal fun messageReportTarget(msg: MessageSnapshot, senderActorHex: String?): FfiReportTarget? {
    if (msg.isOwn) return null
    val plane = msg.planeRef ?: return null
    return reportMessageTarget(
        planeScope = plane.scope,
        recordDigest = plane.recordDigest,
        senderActor = senderActorHex,
        plaintext = msg.body,
    )
}
