package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
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
import org.mockito.ArgumentMatchers.anyLong
import org.mockito.ArgumentMatchers.nullable
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * `AdminDnsVM`'s catch-all/role-address picker source pin (admin.md § 2 →
 * *Which accounts a picker offers*): `hydrate()`
 * (run from `init` the moment the VM is constructed) must read every account
 * on the nest via [ApiClient.adminUsersListAll], never a single
 * `fauna.admin.users.list` page.
 *
 * FFI-free (mocks [ApiClient] itself, mirroring [BackupsVMTest]/[DevicesVMTest]).
 * `Dispatchers.setMain(UnconfinedTestDispatcher())` mirrors [DevicesVMTest] —
 * `init` launches `hydrate()` on `viewModelScope` the moment the VM is built,
 * so [ApiClient.adminUsersListAll] must be stubbed BEFORE construction.
 * Stubbed to an EMPTY list deliberately: `actorOptions`'s default `option`
 * mapper (`com.fauna.ffi.adminPickerOption`) is real native FFI, which can't
 * load in a JVM unit test (`AdminActorOptionsTest`'s own `stubOption` carve-out
 * is the same reason) — mapping over an empty list never invokes it. The
 * handle-else-hex mapping from a populated list is `AdminActorOptionsTest`'s
 * job, not this one's; this test only pins which RPC feeds the picker.
 */
@ExperimentalCoroutinesApi
class AdminDnsVMTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    @Test
    fun hydrateReadsEveryAccountNeverASinglePage() = runTest {
        val api = mock(ApiClient::class.java)
        whenever(api.adminUsersListAll()).thenReturn(emptyList())

        AdminDnsVM(api)

        verify(api).adminUsersListAll()
        verify(api, never()).adminUsersList(nullable(Long::class.java), anyLong())
    }

    /**
     * A bound-id refusal on the cert issue (the nest's `fauna.nest.info` claim
     * disagrees with the connection-bound id) reaches `error-message` through
     * [AdminDnsVM.error] on both entry points — windows/apple/tui already do;
     * android used to skip the dispatch silently. A refusal never reaches the
     * machine, so the VM's own `error` is the only place it can show.
     */
    @Test
    fun boundIdRefusalOnCertIssueReachesErrorMessage() = runTest {
        val api = mock(ApiClient::class.java)
        whenever(api.adminUsersListAll()).thenReturn(emptyList())
        whenever(api.resolveThisNestId()).thenThrow(IllegalStateException("nest id mismatch"))
        val vm = AdminDnsVM(api)

        vm.issueCert("example.com")
        assertEquals("resolve target nest for example.com: nest id mismatch", vm.error.value)

        vm.beginManualIssue("example.org")
        assertEquals("resolve target nest for example.org: nest id mismatch", vm.error.value)
    }
}
