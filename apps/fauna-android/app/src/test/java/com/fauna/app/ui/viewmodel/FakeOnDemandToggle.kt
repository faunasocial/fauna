package com.fauna.app.ui.viewmodel

import com.fauna.app.provider.OnDemandIdentity
import com.fauna.app.provider.OnDemandToggle
import com.fauna.app.provider.OnDemandToggleState
import uniffi.fauna_devices_machine.FolderSummary

/**
 * An in-memory [OnDemandToggle] for the VM tests: a preference map (default
 * ON, like the shared store), a set of refs a watched directory "feeds", and a
 * call log so a test can pin the order of persist → enrol → re-plan.
 */
class FakeOnDemandToggle(
    private val stronger: Set<String> = emptySet(),
    private val failSetShown: Boolean = false,
    private val log: MutableList<String> = mutableListOf(),
) : OnDemandToggle {
    val prefs = mutableMapOf<String, Boolean>()
    val calls: List<String> get() = log

    override suspend fun identify(folder: FolderSummary): OnDemandIdentity {
        val ref = "local:" + folder.id
        return OnDemandIdentity("local%3A" + folder.id + "@" + "aa".repeat(32), ref)
    }

    override suspend fun read(sets: List<OnDemandIdentity>): Map<String, OnDemandToggleState> =
        sets.associate { it.scopedId to OnDemandToggleState(prefs[it.scopedId] ?: true, it.folderRef in stronger) }

    override suspend fun setShown(scopedId: String, shown: Boolean) {
        log += "setShown $scopedId $shown"
        if (failSetShown) error("store unwritable")
        prefs[scopedId] = shown
    }

    override suspend fun replan() {
        log += "replan"
    }
}
