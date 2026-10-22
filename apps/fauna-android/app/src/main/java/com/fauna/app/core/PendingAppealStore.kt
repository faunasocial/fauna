package com.fauna.app.core

import java.util.concurrent.atomic.AtomicReference

/**
 * The one-shot handoff a `dm-message-withheld-badge` tap leaves for the Moderation queue
 * (`moderation.md` § Legal takedown → *App render*, the fourth surface). The badge lives on
 * the conversation detail screen and the appeal form on the queue page — two destinations
 * with their own view models — so the tap stores the queue row's `content_id` here
 * immediately before navigating, and `ModerationQueueVM` consumes it once its queue is
 * loaded and opens the queue's own appeal draft on that row (one flow, the `appeal-*` ids).
 * The twin of web's `lib/pending-appeal.ts`.
 */
object PendingAppealStore {
    private val pending = AtomicReference<String?>(null)

    fun set(contentId: String) = pending.set(contentId)

    /** Returns AND CLEARS the pending `content_id`; `null` when no badge tap routed here. */
    fun consume(): String? = pending.getAndSet(null)
}
