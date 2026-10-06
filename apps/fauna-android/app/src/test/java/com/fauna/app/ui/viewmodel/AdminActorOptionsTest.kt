package com.fauna.app.ui.viewmodel

import com.fauna.ffi.FfiAdminUser
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * The call-site injectivity pin, mirroring apple's
 * `actorLabelStaysInjectiveWhenLabelsCollide` (`AdminDnsVMTests.swift`):
 * [AdminDnsVM]/[AdminWebVM]'s shared `actorOptions` mapper must still
 * produce two DISTINCT option strings for two non-suspended users who share
 * a display `label` but hold distinct handles — the property that broke on
 * windows/apple before the fix.
 *
 * `actorOptions` takes its `option` mapper as an injectable seam
 * (`{ com.fauna.ffi.adminPickerOption(it) }` is only the *default*):
 * Robolectric cannot load the native FFI library the real screens run
 * against (`AdminUsersContentTest`'s `guardianOption` stub is the same
 * carve-out for the guardian family), so this pins the seam's wiring — that
 * both VMs' build sites still route every user through *some* per-item
 * option function rather than the raw `label` — not the native mapping
 * itself, which `fauna_client_admin::admin_picker_option`'s own Rust test
 * already pins.
 */
class AdminActorOptionsTest {

    private fun user(actor: Byte, label: String, handle: String?) = FfiAdminUser(
        actorId = ByteArray(32) { actor },
        tier = "free",
        label = label,
        handle = handle,
        suspended = false,
        createdAt = 0,
        inboxBytesUsed = 0,
        storageBytesUsed = 0,
        eviction = null,
        mailServingEnabled = true,
        isAdmin = false,
    )

    // FFI-free stub matching `fauna_client_admin::admin_picker_option`'s
    // handle-else-full-hex fallback (same shape `AdminUsersContentTest`'s
    // `guardianOption` stub uses).
    private val stubOption: (FfiAdminUser) -> String = {
        it.handle?.takeIf { h -> h.isNotEmpty() } ?: it.actorId.joinToString("") { b -> "%02x".format(b) }
    }

    @Test
    fun actorOptionsStaysInjectiveWhenLabelsCollide() {
        val alex = user(0x11, label = "e2e-test", handle = "alex99")
        val bao = user(0x22, label = "e2e-test", handle = "bao77")

        val options = actorOptions(listOf(alex, bao), option = stubOption)

        assertEquals(2, options.size)
        assertEquals("alex99", options[0].label)
        assertEquals("bao77", options[1].label)
        assertNotEquals(
            "two same-labelled users must still populate two distinct options",
            options[0].label,
            options[1].label,
        )
    }

    @Test
    fun actorOptionsFallsBackToFullHexForAHandleLessAccount() {
        val legacy = user(0x33, label = "Legacy", handle = null)

        val options = actorOptions(listOf(legacy), option = stubOption)

        val hex = legacy.actorId.joinToString("") { b -> "%02x".format(b) }
        assertEquals(hex, options[0].label)
    }
}
