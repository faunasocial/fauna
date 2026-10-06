package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.ffi.FfiAdminUser
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_dns.CertStatusRow
import uniffi.fauna_client_dns.CredentialSummary
import uniffi.fauna_client_dns.DelegationView
import uniffi.fauna_client_dns.DnsAction
import uniffi.fauna_client_dns.DnsCredentialField
import uniffi.fauna_client_dns.DnsManagementMachine
import uniffi.fauna_client_dns.DnsStatus
import uniffi.fauna_client_dns.DomainView
import uniffi.fauna_client_dns.PendingCertIssue
import uniffi.fauna_client_mail_settings.LocalDomainAction
import uniffi.fauna_client_mail_settings.LocalDomainMachine
import uniffi.fauna_client_mail_settings.LocalDomainStatus
import uniffi.fauna_client_mail_settings.LocalDomainView
import uniffi.fauna_client_mail_settings.PrimaryDomainRenameView
import uniffi.fauna_client_mail_settings.RoleAddressKind
import javax.inject.Inject

/** One actor option for the per-domain catch-all picker (admin-dns-domain-catch-all-select). */
data class ActorOption(val idHex: String, val label: String)

/**
 * The `(idHex, label)` pairs an actor picker offers, built from every account
 * on the nest (`fauna_client_admin::users_list_all`, read via
 * [ApiClient.adminUsersListAll] — never a single `fauna.admin.users.list`
 * page) — shared by [AdminDnsVM] (per-domain catch-all/role-address) and
 * [AdminWebVM] (web apex) so a future edit at either build site can't
 * reintroduce the raw, non-unique `label` independently of the other
 * (`admin.md` § 2 → *What identifies a user in an admin picker* / *Which
 * accounts a picker offers*; mirrors `fauna_client_admin::actor_picker_options`).
 * `option` defaults to the real FFI call (`com.fauna.ffi.adminPickerOption`)
 * — overridable so a Robolectric unit test can pin the injectivity property
 * without loading native code.
 */
fun actorOptions(
    users: List<FfiAdminUser>,
    option: (FfiAdminUser) -> String = { com.fauna.ffi.adminPickerOption(it) },
): List<ActorOption> = users.map { ActorOption(HexUtil.bytesToHex(it.actorId), option(it)) }

/**
 * Drives the admin `admin-dns` page (dns-management.md § App surface,
 * mail-multidomain.md § Per-domain catch-all). The page merges TWO shared
 * machines by domain name (the linux/web/windows pattern):
 *   - `DnsManagementMachine` (libs/fauna-client-dns) — the per-domain DNS record
 *     matrix + live red/green verify, the held DNS-provider credentials, and the
 *     managed/manual mode + manage-all master switch (client-held fauna.state.dns
 *     store; the nest never sees the provider key).
 *   - `LocalDomainMachine` (libs/fauna-client-mail-settings) — domain add/remove/
 *     restore CRUD + the per-domain catch-all actor designation.
 * Catch-all + role-address actor options come from every account on the nest
 * (`fauna_client_admin::users_list_all`, admin.md § 2 → *Which accounts a
 * picker offers*). Per priority #2 this view-model holds no DNS / domain
 * logic; it owns the machines, mirrors their snapshots into StateFlows, and
 * dispatches. Linux lead: apps/fauna-linux/src/views/admin.rs build_dns_page.
 */
@HiltViewModel
class AdminDnsVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val dnsMachine: DnsManagementMachine? = api.buildDnsManagementMachineWithCredentials()
    private val ldMachine: LocalDomainMachine? = api.buildLocalDomainsMachine()

    /** Active + soft-deleted domains (CRUD + catch-all + primary badge). */
    val activeDomains = MutableStateFlow<List<LocalDomainView>>(emptyList())
    val removedDomains = MutableStateFlow<List<LocalDomainView>>(emptyList())
    /** The single in-flight primary-domain rename (null when none) + the "Rename
     *  primary" enable hint — both ride the LocalDomainMachine snapshot
     *  (mail-primary-domain-rename.md § UX surface). */
    val activeRename = MutableStateFlow<PrimaryDomainRenameView?>(null)
    val renameAvailable = MutableStateFlow(false)
    /** True only when the next AddDomain would be the deployment's first — it
     *  becomes the primary and can never be removed from any app
     *  (deployment-home-with-public-relay.md § MUA reach). A UX hint only. */
    val addingFirstDomain = MutableStateFlow(false)
    /** Per-domain DNS records (+ verdict) and effective mode, keyed by domain. */
    val dnsDomains = MutableStateFlow<List<DomainView>>(emptyList())
    val credentials = MutableStateFlow<List<CredentialSummary>>(emptyList())
    val manageAll = MutableStateFlow(false)
    val actors = MutableStateFlow<List<ActorOption>>(emptyList())
    /** Per-domain served-cert health (RefreshCertStatus), CNAME renewal-delegations,
     *  and the in-flight manual-paste order — the TLS-cert lifecycle surface
     *  (tls-certificates.md § C.3/C.4 + § B tier 3). */
    val certStatuses = MutableStateFlow<List<CertStatusRow>>(emptyList())
    val delegations = MutableStateFlow<List<DelegationView>>(emptyList())
    val pendingCert = MutableStateFlow<PendingCertIssue?>(null)
    val error = MutableStateFlow<String?>(null)
    val working = MutableStateFlow(false)

    init {
        hydrate()
    }

    private fun hydrate() {
        viewModelScope.launch {
            // Kept: four machine dispatches plus an adminUsersListAll RPC, not
            // a single NestClient RPC (transport.md § Request lifecycle step
            // 3's note). Note DnsAction.VerifyRecords is a mutation, not a
            // read — retrying it on a transient failure is a separate
            // question this row does not resolve.
            repeat(HYDRATE_ATTEMPTS) {
                try {
                    ldMachine?.dispatch(LocalDomainAction.Refresh)
                    dnsMachine?.dispatch(DnsAction.Refresh)
                    dnsMachine?.dispatch(DnsAction.VerifyRecords(domain = null))
                    dnsMachine?.dispatch(DnsAction.RefreshCertStatus)
                    actors.value = actorOptions(api.adminUsersListAll())
                    publish()
                    return@launch
                } catch (_: Exception) {
                    delay(HYDRATE_RETRY_MS)
                }
            }
            publish()
        }
    }

    /** Pull-to-refresh: re-fetch the matrix + credentials, re-verify, re-list domains. */
    fun refresh() = run {
        viewModelScope.launch {
            withWorking {
                ldMachine?.dispatch(LocalDomainAction.Refresh)
                dnsMachine?.dispatch(DnsAction.Refresh)
                dnsMachine?.dispatch(DnsAction.VerifyRecords(domain = null))
                dnsMachine?.dispatch(DnsAction.RefreshCertStatus)
            }
        }
    }

    fun addDomain(domain: String) = dispatchLd(
        LocalDomainAction.AddDomain(
            domain = domain,
            mtaStsCertMode = DEFAULT_CERT_MODE,
        ),
    )

    fun removeDomain(domain: String) = dispatchLd(LocalDomainAction.RemoveDomain(domain = domain))

    fun restoreDomain(domain: String) = dispatchLd(LocalDomainAction.RestoreDomain(domain = domain))

    // ── Primary-domain rename (mail-primary-domain-rename.md § UX surface) ──
    // The nest owns all validation (single-active-rename / new-primary-is-additional
    // / cert-mode / TLS-posture / SAN-cap / grace-not-expired); every dispatch
    // surfaces a refusal via the snapshot error. This VM only forwards the picked
    // target + grace override and reads back the projected state — no rules
    // duplicated (priority #2). Mirrors the web lead
    // (apps/fauna-web/src/routes/admin/dns/+page.svelte, the rename block).

    /** Start a rename, promoting `newPrimaryDomainId` (a picked non-primary
     *  domain's 16-byte id); `graceDays` null → nest default (7). */
    fun startPrimaryRename(newPrimaryDomainId: ByteArray, graceDays: Long?) = dispatchLd(
        LocalDomainAction.StartPrimaryRename(
            newPrimaryDomainId = newPrimaryDomainId,
            graceDays = graceDays,
        ),
    )

    /** Finalize the in-flight rename; `force` completes early from `grace`. */
    fun completePrimaryRename(renameId: ByteArray, force: Boolean) = dispatchLd(
        LocalDomainAction.CompletePrimaryRename(renameId = renameId, force = force),
    )

    /** Push the grace window out by `additionalDays × 1 day` (range [1, 30]). */
    fun extendPrimaryRenameGrace(renameId: ByteArray, additionalDays: Long) = dispatchLd(
        LocalDomainAction.ExtendPrimaryRenameGrace(
            renameId = renameId,
            additionalDays = additionalDays,
        ),
    )

    /** Unwind the in-flight rename (cheap pre-flip; expensive inverse re-flip
     *  post-flip — the confirm dialog names the cost). */
    fun abortPrimaryRename(renameId: ByteArray) = dispatchLd(
        LocalDomainAction.AbortPrimaryRename(renameId = renameId, reason = null),
    )

    /** Designate (`actorIdHex` non-null) or clear ("none") a domain's catch-all. */
    fun setCatchAll(domain: String, actorIdHex: String?) = dispatchLd(
        LocalDomainAction.SetCatchAllActor(
            domain = domain,
            actorId = actorIdHex?.let { HexUtil.hexToBytes(it) },
        ),
    )

    /** Designate (`actorIdHex` non-null) or clear ("Admin (default)") a domain's
     *  per-role override (postmaster/abuse/noc/security). The nest does an atomic
     *  read-merge-write, so setting one role preserves the others
     *  (mail-multidomain.md § Per-domain role-address routing). */
    fun setRoleAddress(domain: String, role: RoleAddressKind, actorIdHex: String?) = dispatchLd(
        LocalDomainAction.SetRoleAddress(
            domain = domain,
            role = role,
            actorId = actorIdHex?.let { HexUtil.hexToBytes(it) },
        ),
    )

    /** Opt one domain in/out of Fauna-managed DNS; the shared machine's SetMode
     *  publishes the records itself on opt-in (matches linux's per-domain toggle). */
    fun setMode(domain: String, managed: Boolean) {
        viewModelScope.launch {
            withWorking {
                dnsMachine?.dispatch(DnsAction.SetMode(domain = domain, managed = managed))
            }
        }
    }

    /** The deployment "Fauna controls DNS" master switch — folds every active
     *  domain at once (ApiClient.dnsSetAllManaged), then re-reads the projection. */
    fun setManageAll(managed: Boolean) {
        viewModelScope.launch {
            withWorking {
                api.dnsSetAllManaged(managed)
                dnsMachine?.dispatch(DnsAction.Refresh)
            }
        }
    }

    fun putCredential(providerId: String, fields: List<DnsCredentialField>, label: String) =
        dispatchDns(DnsAction.PutCredentials(providerId = providerId, fields = fields, label = label))

    fun clearCredential(index: UInt) = dispatchDns(DnsAction.ClearCredentials(index = index))

    fun verifyDomain(domain: String) = dispatchDns(DnsAction.VerifyRecords(domain = domain))

    // ── TLS-cert lifecycle (tls-certificates.md § B tier 2/3 + § C.3/C.4) ──
    // Mirrors linux apps/fauna-linux/src/client.rs dns_{issue_cert,…}.

    /** Get/renew for a **managed/delegated** domain: a single client-driven DNS-01
     *  order (auto-publish + finalize + seal to the serving nest), then re-read the
     *  served-cert badge. `target_nest_id` = the connected nest's own id. */
    fun issueCert(domain: String) {
        viewModelScope.launch {
            withWorking {
                val targetNestId = api.resolveThisNestId()
                if (targetNestId != null) {
                    dnsMachine?.dispatch(DnsAction.IssueCert(domain = domain, targetNestId = targetNestId))
                    dnsMachine?.dispatch(DnsAction.RefreshCertStatus)
                }
            }
        }
    }

    /** Phase 1 of **manual-paste** issuance (a domain with no covering credential):
     *  open the DNS-01 order and surface the transient `_acme-challenge` TXT(s) on
     *  `pendingCert` for the admin to paste. The order is held on the persistent
     *  machine across the paste. */
    fun beginManualIssue(domain: String) {
        viewModelScope.launch {
            withWorking {
                val targetNestId = api.resolveThisNestId()
                if (targetNestId != null) {
                    dnsMachine?.dispatch(
                        DnsAction.BeginManualIssueCert(domain = domain, targetNestId = targetNestId),
                    )
                }
            }
        }
    }

    /** Phase 2 of manual-paste issuance: the admin pasted + verified the TXT —
     *  finalize, seal + deliver, persist the ACME account, then re-read the badge. */
    fun completeManualIssue() {
        viewModelScope.launch {
            withWorking {
                dnsMachine?.dispatch(DnsAction.CompleteManualIssueCert)
                dnsMachine?.dispatch(DnsAction.RefreshCertStatus)
            }
        }
    }

    /** Abandon the suspended manual order — the nest stays on the floor (graceful). */
    fun cancelManualIssue() = dispatchDns(DnsAction.CancelManualIssueCert)

    /** Set up a one-time `_acme-challenge` CNAME renewal-delegation into a held
     *  credential's zone; thereafter renewals auto-publish (no further paste). */
    fun delegateRenewal(domain: String, targetZone: String) =
        dispatchDns(DnsAction.DelegateRenewal(domain = domain, targetZone = targetZone))

    /** Remove a domain's CNAME delegation (reverts to manual paste-per-renewal). */
    fun removeDelegation(domain: String) = dispatchDns(DnsAction.RemoveDelegation(domain = domain))

    /** Turn automatic certificate renewal on/off for `domain` (config-only — no
     *  Refresh, so the toggle stays snappy and the red/green verdicts stay intact;
     *  the opt-OUT persists in `DnsConfig.auto_renew_off`). */
    fun setAutoRenew(domain: String, enabled: Boolean) =
        dispatchDns(DnsAction.SetAutoRenew(domain = domain, enabled = enabled))

    private fun dispatchLd(action: LocalDomainAction) {
        viewModelScope.launch { withWorking { ldMachine?.dispatch(action) } }
    }

    private fun dispatchDns(action: DnsAction) {
        viewModelScope.launch { withWorking { dnsMachine?.dispatch(action) } }
    }

    /** Run a machine interaction with the working flag set, then re-publish the
     *  merged snapshots. Dispatch errors fold into the snapshots' `error` field;
     *  a hard FFI failure leaves the prior snapshots in place. */
    private suspend inline fun withWorking(block: () -> Unit) {
        working.value = true
        try {
            block()
        } catch (_: Exception) {
            // surfaced via snapshot.error
        } finally {
            working.value = false
            publish()
        }
    }

    private fun publish() {
        ldMachine?.let {
            val snap = it.snapshot()
            activeDomains.value = snap.active
            removedDomains.value = snap.softDeleted
            activeRename.value = snap.activeRename
            renameAvailable.value = snap.renameAvailable
            addingFirstDomain.value = snap.addingFirstDomain
        }
        dnsMachine?.let {
            val snap = it.snapshot()
            dnsDomains.value = snap.domains
            credentials.value = snap.credentials
            certStatuses.value = snap.certStatuses
            delegations.value = snap.delegations
            pendingCert.value = snap.pendingCert
            manageAll.value = it.allDomainsManaged(snap.domains.map { d -> d.domain })
        }
        error.value = listOfNotNull(
            ldMachine?.snapshot()?.error,
            dnsMachine?.snapshot()?.error,
        ).firstOrNull()
    }

    private companion object {
        const val HYDRATE_ATTEMPTS = 10
        const val HYDRATE_RETRY_MS = 500L
        // Mirrors libs/fauna-client-mail-settings::local_domains DEFAULT_CERT_MODE.
        // The MTA-STS policy mode is not sent: the nest sets and advances it.
        const val DEFAULT_CERT_MODE = "expand_primary"
    }
}
