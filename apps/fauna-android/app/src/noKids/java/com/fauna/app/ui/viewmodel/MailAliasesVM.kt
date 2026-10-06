package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.AliasKind
import uniffi.fauna_client_mail_settings.AliasesStatus
import uniffi.fauna_client_mail_settings.MailAliasesAction
import uniffi.fauna_client_mail_settings.MailAliasesMachine
import uniffi.fauna_client_mail_settings.MailAliasesSnapshot
import javax.inject.Inject

/**
 * Renders the shared `MailAliasesMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the per-account `mail-aliases` page — list / create / edit / revoke
 * / delete aliases and mint disposables. Per priority #2 this view-model holds
 * **no** alias logic (pattern validation, kind taxonomy, the fixed-order
 * resolver, cross-user uniqueness, caps); it owns the machine, mirrors its
 * snapshot into a StateFlow, and dispatches actions. The backend
 * (`fauna.bridges.*_account_alias` + generate_disposable) is largely built
 * (`mail-aliases.md` § Impl status today) → this page is genuinely green. Linux
 * lead: apps/fauna-linux/src/settings/mail_aliases.rs.
 */
@HiltViewModel
class MailAliasesVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailAliasesMachine? = api.buildMailAliasesMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    // Distinguishes "still loading" from "resolved empty" (`ui/README.md` §
    // Copy comprehensibility rule 5 — an un-hydrated first paint must not
    // claim "No aliases yet", which the machine does not yet know to be
    // true). Flips true exactly once, alongside the first real snapshot
    // publish below (success or retries-exhausted).
    val hydrated = MutableStateFlow(false)

    init {
        hydrate()
    }

    private fun hydrate() {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.hydrate()
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has not landed yet
                // (NestClient::request_inner), and the machine records any real failure
                // in its own snapshot, which the publish below surfaces.
            }
            hydrated.value = true
            publish(m)
        }
    }

    fun create(kind: AliasKind, pattern: String, label: String, spamThreshold: UInt?, ratePerHour: Long?) =
        dispatch(
            MailAliasesAction.Create(
                kind = kind,
                pattern = pattern,
                label = label,
                spamThresholdOverride = spamThreshold,
                rateLimitPerHour = ratePerHour,
            )
        )

    fun generateDisposable(ttlDays: UInt?, uses: UInt?, label: String) =
        dispatch(MailAliasesAction.GenerateDisposable(ttlDays = ttlDays, uses = uses, label = label))

    fun update(aliasIdHex: String, pattern: String, label: String, spamThreshold: UInt?, ratePerHour: Long?) =
        dispatch(
            MailAliasesAction.Update(
                aliasIdHex = aliasIdHex,
                pattern = pattern,
                label = label,
                spamThresholdOverride = spamThreshold,
                rateLimitPerHour = ratePerHour,
            )
        )

    fun revoke(aliasIdHex: String) = dispatch(MailAliasesAction.Revoke(aliasIdHex = aliasIdHex))

    fun enable(aliasIdHex: String) = dispatch(MailAliasesAction.Enable(aliasIdHex = aliasIdHex))

    fun delete(aliasIdHex: String) = dispatch(MailAliasesAction.Delete(aliasIdHex = aliasIdHex))

    fun import(lines: List<String>) = dispatch(MailAliasesAction.Import(lines = lines))

    private fun dispatch(action: MailAliasesAction) {
        val m = machine ?: return
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            publish(m)
        }
    }

    private fun publish(m: MailAliasesMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        val EMPTY_SNAPSHOT = MailAliasesSnapshot(
            aliases = emptyList(),
            defaultDomain = null,
            lastMintedAddress = null,
            lastImportResult = null,
            status = AliasesStatus.IDLE,
            error = null,
        )
    }
}
