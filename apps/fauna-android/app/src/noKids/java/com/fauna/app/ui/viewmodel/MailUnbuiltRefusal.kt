package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.resolveLocalized
import uniffi.fauna_client_mail_settings.mailUsableUnbuilt
import uniffi.fauna_core.Possibility

/**
 * The refusal a mail page's gesture gives when its machine was never built
 * (`docs/goal/ui/README.md` § Offer only what is possible, rule 7: a build
 * failure is never left as a silent control; `mail-settings.md` § Errors &
 * edge cases, *The in-page predicates*). The text is the shared
 * `mail_usable_unbuilt()` answer's reason — the same words the mail-usable
 * entry gate paints — resolved for the page's `error-message` banner, where
 * each mail view-model's `errorMessage` lands. Never a sentence of its own.
 */
internal fun ApiClient.mailUnbuiltRefusal(): String {
    val reason = (mailUsableUnbuilt() as? Possibility.Blocked)?.reason
    return resolveLocalized(context, reason) ?: reason?.key.orEmpty()
}
