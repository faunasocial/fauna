package com.fauna.app.core

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * Bumped by an act that changed the viewer's family relationships — the admin
 * Users hub's take-over (family-safety.md § App surface → *Admin take-over*) —
 * so the Settings family gate's `fauna.family.status` read runs again and
 * `family-tab` reveals the ward. The web twin is the SPA's `familyStatusTick`.
 */
object FamilyStatusEpoch {
    private val _tick = MutableStateFlow(0)
    val tick: StateFlow<Int> = _tick

    fun bump() {
        _tick.value = _tick.value + 1
    }
}
