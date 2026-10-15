package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.CredentialKind
import uniffi.fauna_client_mail_settings.MailSettingsAction
import uniffi.fauna_client_mail_settings.MailSettingsMachine
import uniffi.fauna_client_mail_settings.MailSettingsSnapshot
import uniffi.fauna_client_mail_settings.MuaInstructions
import uniffi.fauna_client_mail_settings.SettingsStatus
import javax.inject.Inject

/**
 * Renders the shared `MailSettingsMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the `mail-settings` hub — enable mail, manage MUA credentials,
 * rotate the MSEK, and read the MUA connection details. Per priority #2 this
 * view-model holds **no** mail logic (wrap/unwrap, KDF, rotation algorithm); it
 * owns the machine, mirrors its snapshot, and dispatches actions. The shared
 * password/token generators (`generate_bridge_password` / `generate_bridge_token`
 * / `warn_manual_bridge_password`, `mail-settings.md` § Impl status) are invoked
 * here — never from the stateless Content — so the credential secret is built in
 * the VM and the screen stays JVM-testable. Linux lead:
 * apps/fauna-linux/src/settings/mail.rs.
 */
@HiltViewModel
class MailSettingsVM @Inject constructor(
    @ApplicationContext private val context: Context,
    private val api: ApiClient,
) : ViewModel() {

    private val machine: MailSettingsMachine? = api.buildMailSettingsMachine()

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)
    val errorMessage = MutableStateFlow<String?>(null)

    /** Read-only deployment storage mode → drives the weak-password warning. */
    // Every nest stores at rest encrypted (no-modes, storage-modes.md), so the
    // manual-password warning's nest_encrypted input is unconditionally true —
    // the same constant tui and apple pass to warn_manual_password.
    val nestEncrypted = MutableStateFlow(true)

    /** The auto-generated PLAIN password shown read-only when auto-generate is on. */
    val generatedPassword = MutableStateFlow("")

    /** The OAUTHBEARER token, shown once after a bearer credential is added. */
    val lastToken = MutableStateFlow<String?>(null)

    // The Forwarding section's two fields (mail-forwarding.md § Per-account
    // "forward all", § Per-account forward rate-limit):
    // each holds the nest-confirmed value as text, repainted from the re-read —
    // never the keystrokes. The ceiling is the nest's, returned beside the limit;
    // the limit commit is inert until it is known.
    val forwardAllTo = MutableStateFlow("")
    val forwardPerHour = MutableStateFlow("")
    val forwardCeiling = MutableStateFlow<UInt?>(null)

    init {
        hydrate()
        hydrateForwarding()
        regeneratePassword()
    }

    private fun hydrateForwarding() {
        viewModelScope.launch {
            try {
                forwardAllTo.value = api.forwardAllToGet().orEmpty()
                publishForwardPerHour(api.forwardPerHourGet())
            } catch (_: Exception) {
                // One attempt: the transport already waits out a socket that has
                // not landed yet (NestClient::request_inner).
            }
        }
    }

    private fun publishForwardPerHour(reply: com.fauna.ffi.ForwardPerHour) {
        forwardPerHour.value = reply.value.toString()
        forwardCeiling.value = reply.ceiling
    }

    /** Commit the forward-all draft. The shared parser decides first: a refused
     *  draft names its reason on the error banner and sends nothing (the nest
     *  would refuse it for the same reason — one rule, priority #2). Blank clears. */
    fun commitForwardAllTo(draft: String) {
        val address = when (val outcome = uniffi.fauna_client_mail_settings.forwardAllToDraftOutcome(draft)) {
            is uniffi.fauna_client_mail_settings.ForwardAllToDraftOutcome.Clear -> null
            is uniffi.fauna_client_mail_settings.ForwardAllToDraftOutcome.Set -> outcome.address
            is uniffi.fauna_client_mail_settings.ForwardAllToDraftOutcome.Refused -> {
                errorMessage.value = com.fauna.app.ui.util.resolveLocalized(context, outcome.message)
                return
            }
        }
        viewModelScope.launch {
            try {
                forwardAllTo.value = api.forwardAllToSet(address).orEmpty()
                errorMessage.value = null
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Commit the hourly-limit draft, bounded by the nest's ceiling. */
    fun commitForwardPerHour(draft: String) {
        val ceiling = forwardCeiling.value ?: return
        val value = when (val outcome = uniffi.fauna_client_mail_settings.forwardPerHourDraftOutcome(draft, ceiling)) {
            is uniffi.fauna_client_mail_settings.ForwardPerHourDraftOutcome.Value -> outcome.value
            is uniffi.fauna_client_mail_settings.ForwardPerHourDraftOutcome.Refused -> {
                errorMessage.value = com.fauna.app.ui.util.resolveLocalized(context, outcome.message)
                return
            }
        }
        viewModelScope.launch {
            try {
                publishForwardPerHour(api.forwardPerHourSet(value))
                errorMessage.value = null
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
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
            publish(m)
        }
    }

    /**
     * Mint a fresh auto-generate PLAIN password at a settled toggle edge (called
     * unconditionally at VM init — the form's default state is PLAIN +
     * autogenerate on — and again when the autogenerate switch flips on). Routes
     * through the shared `resolve_autogenerated_password` mint-once-sequencing
     * decision (mail-credentials.md § Auto-generated bridge password → *Mint-once
     * sequencing*) rather than the raw `generate_bridge_password` free fn, so this
     * is the single settled-edge mint every app now shares. Both call sites
     * only ever ask for the PLAIN+autogenerate-on case, so the result is always
     * `Some`; never call this from `provision()` (submit time) — that would
     * re-mint a different password than what the user copied.
     */
    fun regeneratePassword() {
        runCatching {
            com.fauna.ffi.resolveAutogeneratedBridgePassword(CredentialKind.PLAIN, true)
                ?.let { generatedPassword.value = it }
        }
    }

    fun clearToken() { lastToken.value = null }

    fun enableMail(displayName: String, kind: CredentialKind, autogenerate: Boolean, manualPassword: String) =
        provision(isEnable = true, displayName, kind, autogenerate, manualPassword)

    fun addCredential(displayName: String, kind: CredentialKind, autogenerate: Boolean, manualPassword: String) =
        provision(isEnable = false, displayName, kind, autogenerate, manualPassword)

    private fun provision(
        isEnable: Boolean,
        displayName: String,
        kind: CredentialKind,
        autogenerate: Boolean,
        manualPassword: String,
    ) {
        val name = displayName.ifBlank { "Default" }
        val secret: ByteArray = when (kind) {
            CredentialKind.O_AUTH_BEARER -> {
                val token = runCatching { com.fauna.ffi.generateBridgeToken() }.getOrElse { "" }
                lastToken.value = token
                token.toByteArray(Charsets.UTF_8)
            }
            CredentialKind.PLAIN -> {
                val pw = if (autogenerate) generatedPassword.value else manualPassword
                if (pw.isEmpty()) {
                    errorMessage.value = context.getString(R.string.settings_mail_password_required)
                    return
                }
                pw.toByteArray(Charsets.UTF_8)
            }
        }
        dispatch(
            if (isEnable) MailSettingsAction.EnableMail(displayName = name, kind = kind, secret = secret)
            else MailSettingsAction.AddCredential(displayName = name, kind = kind, secret = secret)
        )
    }

    /**
     * Disable mail via the shared `MailSettingsAction::DisableMail` — bulk
     * soft-revoke every credential (one `RevokeCredential` per row) **and** clear
     * the `fauna.state.mail` MSEK; the snapshot returns to `enabled: false`
     * (mail-settings.md § Disable mail). The shared machine owns the sequence, so
     * the MSEK is cleared too (the old hand-rolled per-credential revoke loop here
     * left it set). It deliberately does **not** call the Admin-scoped,
     * deployment-wide `set_mail_enabled(false)` — that flag boots/tears the
     * box-level s6 mail subsystem shared by every mailbox.
     */
    fun disableMail() = dispatch(MailSettingsAction.DisableMail)

    /** Flip whether this nest serves the user's mailbox over IMAP/CalDAV to
     *  external MUAs (mail-settings.md § Local IMAP/CalDAV-serving toggle). Fires
     *  the caller-scoped `set_mail_serving_enabled` non-optimistically via the
     *  shared machine; the snapshot's `servingEnabled` re-renders the switch. */
    fun setServingEnabled(enabled: Boolean) = dispatch(MailSettingsAction.SetServingEnabled(enabled = enabled))

    /** `onDone` runs once the rotation's dispatch has returned (success or
     *  failure) — the rotate form holds open until then. */
    fun startRotation(excludedCredentials: List<String>, onDone: () -> Unit) =
        dispatch(MailSettingsAction.StartRotation(excludedCredentials = excludedCredentials), onDone)

    fun resumeRotation() = dispatch(MailSettingsAction.ResumeRotation)

    private fun dispatch(action: MailSettingsAction, onDone: () -> Unit = {}) {
        val m = machine ?: return onDone()
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            publish(m)
            onDone()
        }
    }

    private fun publish(m: MailSettingsMachine) {
        val snap = m.snapshot()
        snapshot.value = snap
        errorMessage.value = snap.error
    }

    private companion object {
        val EMPTY_SNAPSHOT = MailSettingsSnapshot(
            enabled = false,
            caldavEnabled = false,
            carddavEnabled = false,
            servesWebdavSet = false,
            credentialManagementReachable = false,
            servingEnabled = true,
            credentials = emptyList(),
            pendingRotation = null,
            mua = MuaInstructions(
                imapHost = "",
                imapPort = 993u,
                smtpHost = "",
                smtpPort = 465u,
                caldavHost = "",
                caldavPort = 443u,
                webdavUrl = "",
                domain = "",
                usernameFormat = "",
                authMechanism = "",
            ),
            status = SettingsStatus.Idle,
            error = null,
        )
    }
}
