package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.ui.util.and
import com.fauna.app.ui.util.machinePossibility
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import com.fauna.app.ui.util.askForm
import uniffi.fauna_core.Possibility
import uniffi.fauna_core.ReadState
import uniffi.fauna_client_mail_settings.ForwarderAction
import uniffi.fauna_client_mail_settings.ForwarderMachine
import uniffi.fauna_client_mail_settings.ForwarderStatus
import uniffi.fauna_client_mail_settings.ForwardersSnapshot
import javax.inject.Inject

/**
 * Renders the shared `ForwarderMachine` (libs/fauna-client-mail-settings, over
 * UniFFI) for the admin `admin-aliases` page — list / create / delete external
 * forwarders. Per priority #2 this view-model holds **no** forwarder logic; it
 * owns the machine, mirrors its snapshot into a StateFlow, and dispatches actions.
 * The backend is built (`admin.md` § 4 Impl status), so actions are genuinely
 * green; rejections surface via `snapshot.error` → the dedicated
 * `admin-aliases-action-error` element. Linux lead:
 * apps/fauna-linux/src/views/admin.rs.
 */
@HiltViewModel
class AdminAliasesVM @Inject constructor(
    private val api: ApiClient,
    @ApplicationContext context: Context,
) : ViewModel() {

    private val machine: ForwarderMachine? = api.buildForwardersMachine()

    // The add form waits for the forwarder and local-domain read (admin.md
    // § Errors & edge cases, *Controls wait for their read*): the shared
    // `ForwarderMachine::add_possibility` — still loading, failed to load,
    // still saving (the VM's own create between press and re-read included).
    private val add = ReadGatedWrite(context) { machinePossibility(machine) { it.addPossibility() } }

    /** The add answer, as a flow the screen collects so a move repaints the
     *  form ([formPossibility] / [createPossibility] fold it). */
    val addAnswer: StateFlow<Possibility> = add.answer

    /** Where the list read stands — the shared `ForwarderMachine::read_state`;
     *  the empty text is painted only once it resolved. A machine that could
     *  not be built never reads. */
    val listRead = MutableStateFlow(machine?.readState() ?: ReadState.FAILED)

    val snapshot = MutableStateFlow(machine?.snapshot() ?: EMPTY_SNAPSHOT)

    /** A refused press's reason, for the page-level `error-message` banner. */
    val errorMessage = MutableStateFlow<String?>(null)

    /** A return by Back to this page reads it again (`AdminEntryReread`). */
    val entryReread = AdminEntryReread(::hydrate)

    init {
        hydrate()
        // A read that has not answered is read again when the nest connection
        // comes back (`docs/goal/behavior/admin.md` § Errors & edge cases,
        // *Controls wait for their read*; tui's `reconnect_reread_op`): only
        // while the shared machine's `read_state` is unresolved, so a draft
        // over an answered page survives.
        viewModelScope.rereadOnReconnect(
            api.reconnectTick,
            answered = { machine?.readState() == ReadState.RESOLVED },
            reread = ::hydrate,
        )
    }

    /** Load the forwarder list + the hosted-domain picker options; retry while
     *  the socket comes up (mirrors [MailSpamVM]). */
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

    /** Whether the add form's picker and fields are live (`admin.md` § 4, *A
     *  forwarder needs a hosted domain*): the machine's shared
     *  `form_possibility` — the read's answer, then blocked naming DNS while
     *  the nest hosts no local domain. */
    fun formPossibility(): Possibility =
        add.current() and (machine?.let { m -> askForm { m.formPossibility() } } ?: Possibility.Possible)

    /** Whether the submit can create on [localDomain] now: the machine's
     *  shared `create_possibility`, which its create asks again. */
    fun createPossibility(localDomain: String): Possibility =
        add.current() and (machine?.let { m -> askForm { m.createPossibility(localDomain) } } ?: Possibility.Possible)

    /** The create asks the add answer live: a press racing the read, or a
     *  second one while the first is on its way, is refused (its reason on the
     *  page's `error-message`, [errorMessage]) and nothing is sent. */
    fun create(localDomain: String, pattern: String, forwardTarget: String) {
        if (!add.begin { errorMessage.value = it }) return
        dispatch(
            ForwarderAction.Create(
                localDomain = localDomain,
                pattern = pattern,
                forwardTarget = forwardTarget,
            ),
            onDone = add::end,
        )
    }

    fun delete(aliasIdHex: String) =
        dispatch(ForwarderAction.Delete(aliasIdHex = aliasIdHex))

    private fun dispatch(action: ForwarderAction, onDone: () -> Unit = {}) {
        val m = machine ?: return onDone()
        viewModelScope.launch {
            try {
                m.dispatch(action)
            } catch (_: Exception) {
                // Dispatch errors are folded into snapshot.error by the machine;
                // a hard FFI failure leaves the prior snapshot in place.
            }
            onDone()
            publish(m)
        }
    }

    private fun publish(m: ForwarderMachine) {
        snapshot.value = m.snapshot()
        listRead.value = m.readState()
        add.refresh()
    }

    private companion object {
        val EMPTY_SNAPSHOT = ForwardersSnapshot(
            forwarders = emptyList(),
            localDomains = emptyList(),
            status = ForwarderStatus.IDLE,
            error = null,
        )
    }
}
