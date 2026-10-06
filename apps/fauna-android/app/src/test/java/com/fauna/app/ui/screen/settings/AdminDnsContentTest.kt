package com.fauna.app.ui.screen.settings

import android.content.Context
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.ui.viewmodel.ActorOption
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_dns.CertHealthState
import uniffi.fauna_client_dns.CertStatusRow
import uniffi.fauna_client_dns.CredentialSummary
import uniffi.fauna_client_dns.DelegationView
import uniffi.fauna_client_dns.DnsCredentialField
import uniffi.fauna_client_dns.DnsRecordRow
import uniffi.fauna_client_dns.DomainView
import uniffi.fauna_client_dns.PendingCertIssue
import uniffi.fauna_client_dns.RecordVerdict
import uniffi.fauna_client_dns.VerifyStatus
import uniffi.fauna_client_mail_settings.DomainDmarcPolicy
import uniffi.fauna_client_mail_settings.LocalDomainView
import uniffi.fauna_client_mail_settings.PrimaryDomainRenameView
import uniffi.fauna_client_mail_settings.RoleAddressKind
import uniffi.fauna_client_mail_settings.RoleAddressOverrideView

/**
 * Compose-level coverage for the stateless [AdminDnsContent] (the admin
 * `admin-dns` page, dns-management.md § App surface): the page controls
 * (refresh / manage-all / add-domain / add-credential), the per-domain record
 * matrix with red/green verdicts + catch-all picker + primary badge + remove,
 * the held-credential list, and the soft-deleted restore list. Renders with
 * seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminDnsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun localDomain(
        domain: String = "example.com",
        primary: Boolean = true,
        catchAll: ByteArray? = null,
        catchAllClearedBySuccessionAt: Long? = null,
        roleOverrides: List<RoleAddressOverrideView> = emptyList(),
        removed: Long? = null,
        domainId: ByteArray = ByteArray(16) { if (primary) 1 else 2 },
    ) = LocalDomainView(
        domainId = domainId,
        domain = domain,
        isPrimary = primary,
        mtaStsMode = "testing",
        mtaStsCertMode = "expand_primary",
        mtaStsMaxAgeSeconds = 604800,
        spfRecord = "v=spf1 mx ~all",
        dkimSelector = null,
        dkimRotationDue = false,
        dkimSelectorActivatedAt = null,
        catchAllActorId = catchAll,
        catchAllClearedBySuccessionAt = catchAllClearedBySuccessionAt,
        roleAddressOverrides = roleOverrides,
        dmarcPolicy = DomainDmarcPolicy.REJECT,
        addedAt = 0,
        removedAt = removed,
    )

    private fun record(
        name: String = "example.com",
        type: String = "MX",
        expected: String = "10 mail.example.com",
        status: VerifyStatus? = VerifyStatus.OK,
    ) = DnsRecordRow(
        name = name,
        recordType = type,
        expected = expected,
        ttlSeconds = 3600u,
        verdict = status?.let { RecordVerdict(observed = listOf(expected), status = it) },
    )

    private fun dnsDomain(
        domain: String = "example.com",
        mode: String = "manual",
        isPrimary: Boolean = false,
        records: List<DnsRecordRow> = listOf(record()),
        autoRenew: Boolean = false,
    ) = DomainView(
        domain = domain,
        mode = mode,
        isPrimary = isPrimary,
        records = records,
        autoRenew = autoRenew,
    )

    private fun certStatus(
        domain: String = "example.com",
        state: CertHealthState = CertHealthState.VALID_TRUSTED,
        notAfterUnix: Long = 4102444800L, // 2100-01-01
        isFloor: Boolean = false,
    ) = CertStatusRow(domain = domain, state = state, notAfterUnix = notAfterUnix, isFloor = isFloor)

    private fun delegation(domain: String = "example.com") = DelegationView(
        domain = domain,
        cname = record(
            name = "_acme-challenge.$domain",
            type = "CNAME",
            expected = "_acme-challenge.$domain.example.net",
            status = null,
        ),
    )

    private fun pendingCert(domain: String = "example.com") = PendingCertIssue(
        domain = domain,
        challenges = listOf(
            record(name = "_acme-challenge.$domain", type = "TXT", expected = "tok123", status = null),
        ),
    )

    /** A projected in-flight rename view (mail-primary-domain-rename.md § UX
     *  surface). Defaults model a `grace`-state rename (post-flip: force-complete +
     *  extend + abort offered), the shape the shared projection produces. */
    private fun renameView(
        state: String = "grace",
        oldPrimary: String = "old.example.com",
        newPrimary: String = "new.example.com",
        graceEndsAt: Long? = 4102444800000L, // 2100-01-01 — far future
        isPostFlipActive: Boolean = true,
        isPreFlip: Boolean = false,
        canComplete: Boolean = false,
        canForceComplete: Boolean = true,
        canExtend: Boolean = true,
        canAbort: Boolean = true,
    ) = PrimaryDomainRenameView(
        renameId = ByteArray(16) { 0xAB.toByte() },
        state = state,
        oldPrimaryDomain = oldPrimary,
        newPrimaryDomain = newPrimary,
        startedAt = 1_700_000_000_000L,
        graceDays = 7,
        graceEndsAt = graceEndsAt,
        readyToCompleteAt = null,
        isPostFlipActive = isPostFlipActive,
        isPreFlip = isPreFlip,
        canComplete = canComplete,
        canForceComplete = canForceComplete,
        canExtend = canExtend,
        canAbort = canAbort,
    )

    private fun render(
        activeDomains: List<LocalDomainView> = emptyList(),
        removedDomains: List<LocalDomainView> = emptyList(),
        activeRename: PrimaryDomainRenameView? = null,
        renameAvailable: Boolean = false,
        addingFirstDomain: Boolean = false,
        dnsDomains: List<DomainView> = emptyList(),
        credentials: List<CredentialSummary> = emptyList(),
        actors: List<ActorOption> = emptyList(),
        manageAll: Boolean = false,
        certStatuses: List<CertStatusRow> = emptyList(),
        delegations: List<DelegationView> = emptyList(),
        pendingCert: PendingCertIssue? = null,
        working: Boolean = false,
        error: String? = null,
        onRefresh: () -> Unit = {},
        onAddDomain: (String) -> Unit = {},
        onRemoveDomain: (String) -> Unit = {},
        onRestoreDomain: (String) -> Unit = {},
        onStartRename: (ByteArray, Long?) -> Unit = { _, _ -> },
        onCompleteRename: (ByteArray, Boolean) -> Unit = { _, _ -> },
        onExtendRename: (ByteArray, Long) -> Unit = { _, _ -> },
        onAbortRename: (ByteArray) -> Unit = {},
        onSetCatchAll: (String, String?) -> Unit = { _, _ -> },
        onSetRoleAddress: (String, RoleAddressKind, String?) -> Unit = { _, _, _ -> },
        onSetMode: (String, Boolean) -> Unit = { _, _ -> },
        onSetManageAll: (Boolean) -> Unit = {},
        onPutCredential: (String, List<DnsCredentialField>, String) -> Unit = { _, _, _ -> },
        onClearCredential: (UInt) -> Unit = {},
        onIssueCert: (String) -> Unit = {},
        onBeginManualIssue: (String) -> Unit = {},
        onCompleteManualIssue: () -> Unit = {},
        onCancelManualIssue: () -> Unit = {},
        onDelegateRenewal: (String, String) -> Unit = { _, _ -> },
        onRemoveDelegation: (String) -> Unit = {},
        onSetAutoRenew: (String, Boolean) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            AdminDnsContent(
                activeDomains = activeDomains,
                removedDomains = removedDomains,
                activeRename = activeRename,
                renameAvailable = renameAvailable,
                addingFirstDomain = addingFirstDomain,
                dnsDomains = dnsDomains,
                credentials = credentials,
                actors = actors,
                manageAll = manageAll,
                certStatuses = certStatuses,
                delegations = delegations,
                pendingCert = pendingCert,
                working = working,
                error = error,
                onBack = {},
                onRefresh = onRefresh,
                onAddDomain = onAddDomain,
                onRemoveDomain = onRemoveDomain,
                onRestoreDomain = onRestoreDomain,
                onStartRename = onStartRename,
                onCompleteRename = onCompleteRename,
                onExtendRename = onExtendRename,
                onAbortRename = onAbortRename,
                onSetCatchAll = onSetCatchAll,
                onSetRoleAddress = onSetRoleAddress,
                onSetMode = onSetMode,
                onSetManageAll = onSetManageAll,
                onPutCredential = onPutCredential,
                onClearCredential = onClearCredential,
                onIssueCert = onIssueCert,
                onBeginManualIssue = onBeginManualIssue,
                onCompleteManualIssue = onCompleteManualIssue,
                onCancelManualIssue = onCancelManualIssue,
                onDelegateRenewal = onDelegateRenewal,
                onRemoveDelegation = onRemoveDelegation,
                onSetAutoRenew = onSetAutoRenew,
            )
        }
    }

    @Test
    fun rendersHeadingAndPageControls() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-refresh-button").assertExists()
        // `error-message` is deliberately ABSENT on a clean page — see
        // [errorMessageIsAbsentUntilThereIsAnError].
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
        composeTestRule.onNodeWithTag("admin-dns-manage-all-toggle").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-credentials-list").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-add-domain-button").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-button").assertExists()
    }

    @Test
    fun domainRowRendersWithRecordsAndPrimaryBadge() {
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain(records = listOf(record(type = "MX"), record(type = "TXT")))),
        )
        composeTestRule.onNodeWithTag("admin-dns-domain").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-name").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-mode").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-catch-all-select").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-primary-badge").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-remove-button").assertExists()
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-dns-record").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-dns-record-status").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-dns-record-copy-button").fetchSemanticsNodes().size)
    }

    @Test
    fun roleAddressPickersRenderAndDispatch() {
        // The per-domain row carries one role-address picker per overridable RFC 2142
        // role; designating an actor on postmaster@ dispatches SetRoleAddress with that
        // role + actor hex (mail-multidomain.md § Per-domain role-address routing).
        var got: Triple<String, RoleAddressKind, String?>? = null
        render(
            activeDomains = listOf(localDomain(domain = "extra.org", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "extra.org")),
            actors = listOf(ActorOption(idHex = "aabb", label = "Alice")),
            onSetRoleAddress = { d, r, a -> got = Triple(d, r, a) },
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-postmaster-select").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-abuse-select").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-noc-select").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-security-select").assertExists()

        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-postmaster-select").performScrollTo().performClick()
        composeTestRule.onNodeWithText("Alice").performClick()
        assertEquals(Triple("extra.org", RoleAddressKind.POSTMASTER, "aabb"), got)
    }

    @Test
    fun roleAddressOverrideShowsDelegatedActor() {
        // A pre-set abuse@ override renders the delegated actor's label in that picker
        // (read path off LocalDomainView.roleAddressOverrides).
        render(
            activeDomains = listOf(
                localDomain(
                    domain = "extra.org",
                    primary = false,
                    roleOverrides = listOf(
                        RoleAddressOverrideView(
                            role = RoleAddressKind.ABUSE,
                            actorId = byteArrayOf(0xAA.toByte(), 0xBB.toByte()),
                        ),
                    ),
                ),
            ),
            dnsDomains = listOf(dnsDomain(domain = "extra.org")),
            actors = listOf(ActorOption(idHex = "aabb", label = "Bob")),
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-role-address-abuse-select")
            .performScrollTo()
            .assertTextContains("Bob", substring = true)
    }

    @Test
    fun primaryDomainCannotBeRemoved() {
        render(activeDomains = listOf(localDomain(primary = true)), dnsDomains = listOf(dnsDomain()))
        composeTestRule.onNodeWithTag("admin-dns-domain-remove-button").assertIsNotEnabled()
    }

    @Test
    fun nonPrimaryDomainRemoveFires() {
        var removed: String? = null
        render(
            activeDomains = listOf(localDomain(domain = "extra.org", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "extra.org")),
            onRemoveDomain = { removed = it },
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-remove-button").performScrollTo().performClick()
        assertEquals("extra.org", removed)
    }

    @Test
    fun modeToggleFires() {
        var got: Pair<String, Boolean>? = null
        render(
            activeDomains = listOf(localDomain(domain = "extra.org", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "extra.org", mode = "manual")),
            onSetMode = { d, m -> got = d to m },
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-mode").performScrollTo().performClick()
        assertEquals("extra.org" to true, got)
    }

    @Test
    fun manageAllToggleFires() {
        var managed: Boolean? = null
        render(manageAll = false, onSetManageAll = { managed = it })
        composeTestRule.onNodeWithTag("admin-dns-manage-all-toggle").performScrollTo().performClick()
        assertEquals(true, managed)
    }

    @Test
    fun addDomainRevealsFormAndSubmits() {
        var added: String? = null
        render(onAddDomain = { added = it })
        composeTestRule.onNodeWithTag("admin-dns-add-domain-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-add-domain-input").performTextInput("New.Example.COM")
        composeTestRule.onNodeWithTag("admin-dns-add-domain-submit-button").performScrollTo().performClick()
        assertEquals("new.example.com", added)
    }

    // A domainless nest's first add is a one-way door (`deployment-home-with-
    // public-relay.md` § MUA reach): the primary it becomes can never be
    // removed from any app. No testTag on the warning — untagged chrome, not a
    // ui.yaml id — so it is asserted by its rendered text.
    @Test
    fun firstDomainAddWarnsOfIrreversibilityOnlyWhenDomainless() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        render(addingFirstDomain = true)
        composeTestRule.onNodeWithTag("admin-dns-add-domain-button").performScrollTo().performClick()
        composeTestRule.onNodeWithText(
            ctx.getString(R.string.admin_dns_add_domain_primary_warning),
        ).assertExists()
    }

    @Test
    fun secondDomainAddDoesNotWarn() {
        val ctx = ApplicationProvider.getApplicationContext<Context>()
        render(addingFirstDomain = false)
        composeTestRule.onNodeWithTag("admin-dns-add-domain-button").performScrollTo().performClick()
        composeTestRule.onNodeWithText(
            ctx.getString(R.string.admin_dns_add_domain_primary_warning),
        ).assertDoesNotExist()
    }

    @Test
    fun addCredentialRevealsProviderRowAndForm() {
        render()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-provider-row").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-provider-row[cloudflare]").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-form").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-submit-button").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-add-credential-cancel-button").assertExists()
    }

    @Test
    fun credentialItemRendersAndClearFires() {
        var cleared: UInt? = null
        render(
            credentials = listOf(CredentialSummary(providerId = "hetzner", zones = listOf("example.com"), label = "Hetzner")),
            onClearCredential = { cleared = it },
        )
        composeTestRule.onNodeWithTag("admin-dns-credential-item").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-credential-item-provider").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-credential-item-zones").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-credential-item-clear-button").performScrollTo().performClick()
        assertEquals(0u, cleared)
    }

    @Test
    fun removedDomainRestoreFires() {
        var restored: String? = null
        render(
            removedDomains = listOf(localDomain(domain = "old.example.com", primary = false, removed = 1L)),
            onRestoreDomain = { restored = it },
        )
        composeTestRule.onNodeWithTag("admin-dns-removed-domain").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-removed-domain-name").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-removed-domain-restore-button").performScrollTo().performClick()
        assertEquals("old.example.com", restored)
    }

    @Test
    fun emptyStateRendersWhenNoDomains() {
        render()
        composeTestRule.onNodeWithTag("admin-dns-domain").assertDoesNotExist()
    }

    @Test
    fun errorRenders() {
        render(error = "list_records failed")
        composeTestRule.onNodeWithTag("error-message").assertTextEquals("list_records failed")
    }

    /**
     * `e2e-conventions.md` convention 2's rider, obligation (a): a shim carrying
     * the shared `error-message` id must LEAVE the tree when it has nothing to
     * say. This page used to render an empty `Box` under the id whenever [error]
     * was null, which made `is_visible("error-message")` structurally true on a
     * clean page — so the `assert not is_visible("error-message")` that every
     * such e2e test opens with could never fail (the bridge resolves visibility
     * as bare existence: `ElementOps.isVisible` = `findAll(id).isNotEmpty()`).
     */
    @Test
    fun errorMessageIsAbsentUntilThereIsAnError() {
        render()
        composeTestRule.onNodeWithTag("error-message").assertDoesNotExist()
    }

    // ── TLS-cert lifecycle (tls-certificates.md § C.4 + § B tier 2/3) ──

    @Test
    fun certStatusBadgeShowsValidTrusted() {
        // A CA-issued (trusted) cert renders the "Valid" badge with the expiry date.
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            certStatuses = listOf(certStatus(state = CertHealthState.VALID_TRUSTED)),
        )
        composeTestRule.onNodeWithTag("admin-dns-cert-status").assertTextContains("Valid", substring = true)
    }

    @Test
    fun certStatusBadgeShowsSelfSignedOnFloor() {
        // On the self-signed floor → "Renew needed" + a self-signed sub-label.
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            certStatuses = listOf(
                certStatus(state = CertHealthState.ON_FLOOR_RENEW_NEEDED, isFloor = true),
            ),
        )
        composeTestRule.onNodeWithTag("admin-dns-cert-status").assertTextContains("self-signed", substring = true)
    }

    @Test
    fun certStatusBadgeChecksWhenNoStatusYet() {
        // Until RefreshCertStatus returns, the badge reads "Checking…".
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
        )
        composeTestRule.onNodeWithTag("admin-dns-cert-status").assertTextContains("Checking", substring = true)
    }

    @Test
    fun issueButtonBeginsManualOrderForManualDomain() {
        var began: String? = null
        var issued: String? = null
        render(
            activeDomains = listOf(localDomain(domain = "extra.org", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "extra.org", mode = "manual")),
            onBeginManualIssue = { began = it },
            onIssueCert = { issued = it },
        )
        composeTestRule.onNodeWithTag("admin-dns-cert-issue-button").performScrollTo().performClick()
        assertEquals("extra.org", began)
        assertEquals(null, issued)
    }

    @Test
    fun issueButtonIssuesDirectlyForManagedDomain() {
        var issued: String? = null
        render(
            activeDomains = listOf(localDomain(domain = "extra.org", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "extra.org", mode = "managed")),
            onIssueCert = { issued = it },
        )
        composeTestRule.onNodeWithTag("admin-dns-cert-issue-button").performScrollTo().performClick()
        assertEquals("extra.org", issued)
    }

    @Test
    fun manualPasteSurfaceRendersWhenOrderPending() {
        var completed = false
        var cancelled = false
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            pendingCert = pendingCert(),
            onCompleteManualIssue = { completed = true },
            onCancelManualIssue = { cancelled = true },
        )
        // The get/renew button is inert while an order is pending.
        composeTestRule.onNodeWithTag("admin-dns-cert-issue-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-dns-cert-complete-button").performScrollTo().performClick()
        assertEquals(true, completed)
        composeTestRule.onNodeWithTag("admin-dns-cert-cancel-button").performScrollTo().performClick()
        assertEquals(true, cancelled)
    }

    @Test
    fun autoRenewCheckboxShownForManagedAndToggleFires() {
        var got: Pair<String, Boolean>? = null
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain(mode = "managed", autoRenew = true)),
            onSetAutoRenew = { d, on -> got = d to on },
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-auto-renew").performScrollTo().performClick()
        // Was on (default) → toggling opts out.
        assertEquals("example.com" to false, got)
    }

    @Test
    fun autoRenewCheckboxHiddenForManualUndelegatedDomain() {
        render(
            activeDomains = listOf(localDomain(domain = "extra.org", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "extra.org", mode = "manual")),
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-auto-renew").assertDoesNotExist()
    }

    @Test
    fun delegateFormRevealsAndSubmits() {
        var got: Pair<String, String>? = null
        render(
            activeDomains = listOf(localDomain(domain = "home.example.com", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "home.example.com", mode = "manual")),
            credentials = listOf(
                CredentialSummary(providerId = "hetzner", zones = listOf("example.net"), label = "H"),
            ),
            onDelegateRenewal = { d, z -> got = d to z },
        )
        composeTestRule.onNodeWithTag("admin-dns-cert-delegate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-cert-delegate-zone-select").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-cert-delegate-submit-button").performScrollTo().performClick()
        assertEquals("home.example.com" to "example.net", got)
    }

    @Test
    fun delegatedDomainShowsRemoveAndCname() {
        var removed: String? = null
        render(
            activeDomains = listOf(localDomain(domain = "home.example.com", primary = false)),
            dnsDomains = listOf(dnsDomain(domain = "home.example.com", mode = "manual")),
            delegations = listOf(delegation(domain = "home.example.com")),
            onRemoveDelegation = { removed = it },
        )
        // A delegated (but unmanaged) domain shows the auto-renew checkbox too.
        composeTestRule.onNodeWithTag("admin-dns-domain-auto-renew").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-cert-remove-delegation-button").performScrollTo().performClick()
        assertEquals("home.example.com", removed)
    }

    // ── Catch-all succession-cleared state (succession-aftermath.md
    // § Re-key scope) ──

    @Test
    fun catchAllClearedStateAbsentWhenNeverClearedBySuccession() {
        render(
            activeDomains = listOf(localDomain(catchAllClearedBySuccessionAt = null)),
            dnsDomains = listOf(dnsDomain()),
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-catch-all-cleared-state").assertDoesNotExist()
    }

    @Test
    fun catchAllClearedStateRendersOnlyWhenSuccessionCleared() {
        render(
            activeDomains = listOf(localDomain(catchAllClearedBySuccessionAt = 1_700_000_000_000)),
            dnsDomains = listOf(dnsDomain()),
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-catch-all-cleared-state").assertExists()
    }

    // ── Primary-domain rename (mail-primary-domain-rename.md § UX surface) ──

    @Test
    fun renamePrimaryButtonDisabledWhenUnavailable() {
        // The two-step rule: with no active non-primary domain, "Rename primary"
        // renders on the primary row but is disabled (rename_available == false).
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            renameAvailable = false,
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-rename-button").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-domain-rename-button").assertIsNotEnabled()
    }

    @Test
    fun renamePrimaryButtonOpensAndCancelsSheet() {
        render(
            activeDomains = listOf(
                localDomain(domain = "old.example.com", primary = true, domainId = ByteArray(16) { 1 }),
                localDomain(domain = "new.example.com", primary = false, domainId = ByteArray(16) { 2 }),
            ),
            dnsDomains = listOf(dnsDomain(domain = "old.example.com"), dnsDomain(domain = "new.example.com")),
            renameAvailable = true,
        )
        composeTestRule.onNodeWithTag("admin-dns-domain-rename-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-sheet").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-rename-new-primary-select").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-rename-grace-days-input").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-rename-cancel-button").performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-sheet").assertDoesNotExist()
    }

    @Test
    fun promoteButtonPreTargetsAndSubmitsRename() {
        var got: Pair<ByteArray, Long?>? = null
        val newId = ByteArray(16) { 7 }
        render(
            activeDomains = listOf(
                localDomain(domain = "old.example.com", primary = true, domainId = ByteArray(16) { 1 }),
                localDomain(domain = "new.example.com", primary = false, domainId = newId),
            ),
            dnsDomains = listOf(dnsDomain(domain = "old.example.com"), dnsDomain(domain = "new.example.com")),
            renameAvailable = true,
            onStartRename = { id, days -> got = id to days },
        )
        // "Promote to primary" pre-targets this row; submit dispatches its id +
        // null grace (blank input → nest default 7).
        composeTestRule.onNodeWithTag("admin-dns-domain-promote-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-submit-button").performClick()
        assertEquals(newId.toList(), got?.first?.toList())
        assertEquals(null, got?.second)
    }

    @Test
    fun renameBannerRendersGraceControlsAndStateBadge() {
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            activeRename = renameView(state = "grace"),
            renameAvailable = false,
        )
        composeTestRule.onNodeWithTag("admin-dns-rename-banner").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-rename-complete-button").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-rename-extend-button").assertExists()
        composeTestRule.onNodeWithTag("admin-dns-rename-abort-button").assertExists()
        // The primary row shows the in-flight state badge.
        composeTestRule.onNodeWithTag("admin-dns-domain-rename-state").assertExists()
    }

    @Test
    fun renameCompleteRevealsConfirmAndForceCompletes() {
        var got: Pair<ByteArray, Boolean>? = null
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            activeRename = renameView(state = "grace", canForceComplete = true),
            onCompleteRename = { id, force -> got = id to force },
        )
        composeTestRule.onNodeWithTag("admin-dns-rename-complete-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-complete-confirm-button").performScrollTo().performClick()
        // Still in grace → force complete.
        assertEquals(true, got?.second)
    }

    @Test
    fun renameExtendFiresWithInputDays() {
        var days: Long? = null
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            activeRename = renameView(state = "grace", canExtend = true),
            onExtendRename = { _, d -> days = d },
        )
        // Default extend input is "7".
        composeTestRule.onNodeWithTag("admin-dns-rename-extend-button").performScrollTo().performClick()
        assertEquals(7L, days)
    }

    @Test
    fun renameAbortRevealsConfirmAndFires() {
        var aborted = false
        render(
            activeDomains = listOf(localDomain(primary = true)),
            dnsDomains = listOf(dnsDomain()),
            activeRename = renameView(state = "grace"),
            onAbortRename = { aborted = true },
        )
        composeTestRule.onNodeWithTag("admin-dns-rename-abort-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-dns-rename-abort-confirm-button").performScrollTo().performClick()
        assertEquals(true, aborted)
    }
}
