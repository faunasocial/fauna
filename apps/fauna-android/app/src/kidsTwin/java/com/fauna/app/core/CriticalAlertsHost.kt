package com.fauna.app.core

import javax.inject.Inject
import javax.inject.Singleton

/**
 * The cross-page critical-alerts registry — **the excised half**, compiled into
 * the `kids` build type only in place of the `src/noKids/` twin
 * (`dynamic-features.md` § Compile-time excision). The registry's only feeder
 * is the Bluesky settings machine (`critical-alerts.md` § Mechanism), which the
 * kids `fauna-ffi` flavor compiles out together with `fauna_client_alerts`
 * itself, so this flavor has no alert to hold and no banner to paint.
 *
 * Only [clearAll] survives, because [ApiClient.clearAuth] — android's one
 * identity-teardown funnel — calls it on every flavor.
 */
@Singleton
class CriticalAlertsHost @Inject constructor() {
    /** Excised — there is never an alert to clear in this flavor. */
    fun clearAll() = Unit
}
