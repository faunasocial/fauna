package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiAdminUser
import com.fauna.ffi.FfiAdminUsersListReply
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever

/**
 * `AdminUsersVM.allUsers`'s picker source pin (admin.md § 2 → *Which accounts
 * a picker offers*): the guardian pickers
 * (Pending requests / Invite sections) read every account on the nest via
 * [ApiClient.adminUsersListAll], kept separate from the paginated [users]
 * page — and refreshed alongside it on [AdminUsersVM.refresh] and every
 * post-row-action refetch, mirroring apple's `loadAllUsers` beside every
 * `loadUsers`.
 *
 * FFI-free (mocks [ApiClient] itself, mirroring [BackupsVMTest]) — unlike
 * [AdminDnsVMTest]/[AdminWebVMTest] this VM never maps [FfiAdminUser] through
 * the native `adminPickerOption`, so the seeded list content itself is
 * asserted directly, no empty-list dodge needed.
 */
@ExperimentalCoroutinesApi
class AdminUsersVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private fun user(actor: Byte, handle: String) = FfiAdminUser(
        actorId = ByteArray(32) { actor },
        tier = "free",
        label = handle,
        handle = handle,
        suspended = false,
        createdAt = 0,
        inboxBytesUsed = 0,
        storageBytesUsed = 0,
        eviction = null,
        mailServingEnabled = true,
        isAdmin = false,
    )

    @Test
    fun refreshPopulatesAllUsersSeparatelyFromThePaginatedPage() = runTest {
        val api = mock(ApiClient::class.java)
        val onPage = user(0x11, "alex99")
        // A guardian who exists on the nest but is NOT on the current
        // (single-row) Users page — only `adminUsersListAll` can see them.
        val offPage = user(0x22, "bao77")
        whenever(api.adminUsersList(AdminUsersVM.PAGE_SIZE, 0L)).thenReturn(FfiAdminUsersListReply(listOf(onPage), 2))
        whenever(api.adminUsersListAll()).thenReturn(listOf(onPage, offPage))
        val vm = AdminUsersVM(api)

        vm.refresh()

        assertEquals(listOf(onPage), vm.users.value)
        assertEquals(listOf(onPage, offPage), vm.allUsers.value)
    }

    @Test
    fun rowActionRefetchesAllUsersToo() = runTest {
        val api = mock(ApiClient::class.java)
        val alice = user(0x33, "alice")
        val guardian = user(0x44, "guardy99")
        whenever(api.adminUsersList(AdminUsersVM.PAGE_SIZE, 0L)).thenReturn(FfiAdminUsersListReply(listOf(alice), 1))
        whenever(api.adminUsersListAll()).thenReturn(listOf(alice, guardian))
        val vm = AdminUsersVM(api)
        vm.refresh()
        assertEquals(listOf(alice, guardian), vm.allUsers.value)

        // A new guardian is admitted after the initial load — setUserTier's
        // reloadUsers() refetch must pick it up too, not just the paginated
        // page (mirrors the `_users` reload it already did).
        val newGuardian = user(0x55, "newguardy")
        whenever(api.adminUsersListAll()).thenReturn(listOf(alice, guardian, newGuardian))
        vm.setUserTier(alice, "personal")

        assertEquals(listOf(alice, guardian, newGuardian), vm.allUsers.value)
    }
}
