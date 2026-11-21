package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.pressAsked
import com.fauna.ffi.readPossibility
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.CredentialKind
import uniffi.fauna_client_mail_settings.MailSettingsAction
import uniffi.fauna_client_mail_settings.MailSettingsMachine
import uniffi.fauna_client_mail_settings.MailSettingsSnapshot
import uniffi.fauna_client_mail_settings.MuaInstructions
import uniffi.fauna_client_mail_settings.SettingsStatus
import uniffi.fauna_client_mail_settings.mailSettingsEnableTogglePossibility
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState
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

    // The session's shared machine, the one [MailUsableVM] reads, so an enable
    // here is the snapshot the mail-usable gate answers from.
    private val machine: MailSettingsMachine? = api.sharedMailSettingsMachine()

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

    // The receive / send switches, the automatic reply and the mailbox quota
    // (mail-policy-config.md § Tier 3; email-filters.md § The auto-respond
    // setting; imap-server.md § Composition with submission quota). Each is
    // `null` until its first read lands, so the ids are absent until then; the
    // switches and the reply repaint from the nest's re-read, never the tap.
    val switches = MutableStateFlow<com.fauna.ffi.MailSwitches?>(null)
    val autorespond = MutableStateFlow<com.fauna.ffi.FfiAutoRespond?>(null)
    val autorespondMessage = MutableStateFlow("")
    val quotas = MutableStateFlow<uniffi.fauna_client_mail_settings.MailQuotas?>(null)

    // Controls wait for their reads (mail-settings.md § Errors & edge cases,
    // *Controls wait for their reads*; tui's `forward_all_read` /
    // `autorespond_read`): where the forward-all and automatic-reply reads
    // stand, held beside the reads they track. Each control's answer is the
    // shared `Possibility::from_read` over it, so an empty pre-read draft
    // never clears a forward and never writes a reply filter.
    private var forwardAllRead = ReadState.PENDING
    private var autorespondRead = ReadState.PENDING
    private var forwardPerHourRead = ReadState.PENDING
    private val _forwardPerHourAnswer = MutableStateFlow(readPossibility(ReadState.PENDING))
    private val _forwardAllAnswer = MutableStateFlow(readPossibility(ReadState.PENDING))
    private val _autorespondAnswer = MutableStateFlow(readPossibility(ReadState.PENDING))

    /** `mail-settings-forward-all-to-input` and its commit. */
    val forwardAllAnswer: StateFlow<Possibility> = _forwardAllAnswer.asStateFlow()

    /** `mail-settings-forward-per-hour-input` and its commit: waits for the
     *  read that brings the ceiling bounding the value (tui's
     *  `forward_per_hour_answer`). */
    val forwardPerHourAnswer: StateFlow<Possibility> = _forwardPerHourAnswer.asStateFlow()

    /** `mail-settings-autorespond-message-input` and its commit. */
    val autorespondAnswer: StateFlow<Possibility> = _autorespondAnswer.asStateFlow()

    private fun forwardAllReadAs(read: ReadState) {
        forwardAllRead = read
        _forwardAllAnswer.value = readPossibility(read)
    }

    private fun forwardPerHourReadAs(read: ReadState) {
        forwardPerHourRead = read
        _forwardPerHourAnswer.value = readPossibility(read)
    }

    private fun autorespondReadAs(read: ReadState) {
        autorespondRead = read
        _autorespondAnswer.value = readPossibility(read)
    }

    /** `mail-settings-enabled-toggle`'s answer, the shared
     *  `MailSettingsSnapshot::enable_toggle_possibility` over the machine's
     *  snapshot: still loading before the first hydrate (the never-read
     *  default carries `read = false`). */
    fun enableAnswer(): Possibility = mailSettingsEnableTogglePossibility(snapshot.value)

    /** Rule 6 at the press: ask [answer] again; a blocked one names its
     *  reason on `error-message` and sends nothing. */
    private inline fun asked(answer: Possibility, go: () -> Unit) =
        pressAsked(context, answer, onRefuse = { errorMessage.value = it }, go = go)

    init {
        hydrate()
        hydrateForwarding()
        hydrateAccountControls()
        regeneratePassword()
    }

    private fun hydrateAccountControls() {
        viewModelScope.launch {
            try { switches.value = api.mailSwitchesGet() } catch (_: Exception) { }
            try {
                publishAutorespond(api.autorespondGet())
                autorespondReadAs(ReadState.RESOLVED)
            } catch (_: Exception) {
                autorespondReadAs(afterFailedRead(autorespondRead))
            }
            try { quotas.value = api.mailQuotasGet() } catch (_: Exception) { }
        }
    }

    private fun publishAutorespond(reply: com.fauna.ffi.FfiAutoRespond) {
        autorespond.value = reply
        if (reply.enabled) autorespondMessage.value = reply.message
    }

    /** Flip the receive switch from the last confirmed read; inert before one. */
    fun toggleReceive() {
        val current = switches.value ?: return
        setSwitches(receive = !current.receiveEnabled, send = null)
    }

    /** Flip the send switch from the last confirmed read; inert before one. */
    fun toggleSend() {
        val current = switches.value ?: return
        setSwitches(receive = null, send = !current.sendEnabled)
    }

    /** Flip the scan-result-row switch from the last confirmed read; inert
     *  before one. The nest's re-read repaints all three switches. */
    fun toggleScanResultVisible() {
        val current = switches.value ?: return
        viewModelScope.launch {
            try {
                switches.value = api.mailScanResultVisibleSet(!current.scanResultVisible)
                errorMessage.value = null
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private fun setSwitches(receive: Boolean?, send: Boolean?) {
        viewModelScope.launch {
            try {
                switches.value = api.mailSwitchesSet(receive, send)
                errorMessage.value = null
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Edit the reply's draft; committed by [commitAutorespondMessage]. */
    fun editAutorespondMessage(draft: String) { autorespondMessage.value = draft }

    /** The toggle: while the reply is on it turns it off; otherwise it is the
     *  message commit (the draft check first). */
    fun toggleAutorespond() = asked(_autorespondAnswer.value) {
        if (autorespond.value?.enabled == true) {
            viewModelScope.launch {
                try {
                    api.autorespondClear()
                    publishAutorespond(api.autorespondGet())
                    errorMessage.value = null
                } catch (e: Exception) {
                    errorMessage.value = e.message
                }
            }
        } else {
            commitAutorespondMessage()
        }
    }

    /** Commit the message draft, which also turns the reply on. The shared
     *  check decides first: a blank draft names its reason on the error banner
     *  and sends nothing — and before that, the reply's read: a pre-read
     *  commit refuses with the read's reason. */
    fun commitAutorespondMessage() = asked(_autorespondAnswer.value) { sendAutorespondMessage() }

    private fun sendAutorespondMessage() {
        val text = when (val outcome = uniffi.fauna_client_mail_settings.autorespondMessageDraftOutcome(autorespondMessage.value)) {
            is uniffi.fauna_client_mail_settings.AutorespondMessageDraftOutcome.Set -> outcome.text
            is uniffi.fauna_client_mail_settings.AutorespondMessageDraftOutcome.Refused -> {
                errorMessage.value = com.fauna.app.ui.util.resolveLocalized(context, outcome.message)
                return
            }
        }
        viewModelScope.launch {
            try {
                api.autorespondSet(text)
                publishAutorespond(api.autorespondGet())
                errorMessage.value = null
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    private fun hydrateForwarding() {
        viewModelScope.launch {
            // One attempt each: the transport already waits out a socket that
            // has not landed yet (NestClient::request_inner). The forward-all
            // read is tracked on its own, so its control waits for it alone.
            try {
                forwardAllTo.value = api.forwardAllToGet().orEmpty()
                forwardAllReadAs(ReadState.RESOLVED)
            } catch (_: Exception) {
                forwardAllReadAs(afterFailedRead(forwardAllRead))
            }
            try {
                publishForwardPerHour(api.forwardPerHourGet())
            } catch (_: Exception) {
                forwardPerHourReadAs(afterFailedRead(forwardPerHourRead))
            }
        }
    }

    private fun publishForwardPerHour(reply: com.fauna.ffi.ForwardPerHour) {
        forwardPerHour.value = reply.value.toString()
        forwardCeiling.value = reply.ceiling
        // The ceiling is only ever set from a read.
        forwardPerHourReadAs(ReadState.RESOLVED)
    }

    /** Commit the forward-all draft. The shared parser decides first: a refused
     *  draft names its reason on the error banner and sends nothing (the nest
     *  would refuse it for the same reason — one rule, priority #2). Blank clears.
     *  Before that, the forward-all read: a pre-read commit refuses with the
     *  read's reason, so an empty pre-read draft never clears a forward. */
    fun commitForwardAllTo(draft: String) = asked(_forwardAllAnswer.value) { sendForwardAllTo(draft) }

    private fun sendForwardAllTo(draft: String) {
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

    /** Commit the hourly-limit draft, bounded by the nest's ceiling. Before
     *  the read that brings the ceiling, the commit refuses with the read's
     *  reason (still loading, or failed to load) — never silently. */
    fun commitForwardPerHour(draft: String) = asked(_forwardPerHourAnswer.value) { sendForwardPerHour(draft) }

    private fun sendForwardPerHour(draft: String) {
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

    /** The enable form's submit asks the enable toggle's answer again
     *  (rule 6). A never-built machine is [dispatch]'s unbuilt refusal (rule 7),
     *  not a "still loading" that would never end. */
    fun enableMail(displayName: String, kind: CredentialKind, autogenerate: Boolean, manualPassword: String) {
        if (machine == null) return provision(isEnable = true, displayName, kind, autogenerate, manualPassword)
        asked(enableAnswer()) { provision(isEnable = true, displayName, kind, autogenerate, manualPassword) }
    }

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
        // Rule 7 (ui/README.md § Offer only what is possible): a gesture whose
        // machine was never built says so on `error-message`, never silently.
        val m = machine ?: run {
            errorMessage.value = api.mailUnbuiltRefusal()
            return onDone()
        }
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
        /** A read that failed keeps an earlier success (`Possibility::from_read`'s
         *  rule; tui's `after_read`): the page still holds a value the nest
         *  confirmed. */
        fun afterFailedRead(was: ReadState): ReadState =
            if (was == ReadState.RESOLVED) ReadState.RESOLVED else ReadState.FAILED

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
