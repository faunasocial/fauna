package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.RecoveryKitUiState
import com.fauna.app.ui.viewmodel.RecoveryKitVM
import com.fauna.ffi.FfiRecoveryKitStatus
import com.fauna.ffi.FfiSweepCopy
import com.fauna.ffi.FfiSweepView
import social.fauna.generated.Ids
import uniffi.fauna_core.QrMatrix

/**
 * The Settings/Account **Recovery kit** section (`docs/goal/ui/settings.md`
 * § Recovery kit) — the RecoveryKey's Settings home, placed immediately after
 * [IdentityExportSection]: the two are siblings, each revealing a root secret
 * once, as 64-hex + QR, and neither persisting anything.
 *
 * The thin stateful half: it hydrates [RecoveryKitVM] for the signed-in
 * identity, routes the VM's error sentence onto the page's `error-message`
 * banner, and drops every held secret when the section leaves composition —
 * there is deliberately no "show it again" path (`identity-succession.md`
 * § The RecoveryKey — *Custody*). Everything visible is
 * [RecoveryKitSectionContent].
 *
 * @param sessionActorIdHex the signed-in identity — **one value, two roles**. At
 *   ceremony time it is the identity the account is moving *away* from, so the
 *   succession records which registry row the retired identity is before the
 *   switch makes it unnameable; at hydrate time it is whoever is signed in now,
 *   which is what lets the owed kit be claimed by the successor's session and
 *   refused to the departing one. A change re-scopes the whole section.
 * @param onSucceeded switch to the successor (its actor id) after a succession
 *   the device managed to persist — an account *switch*, never the sign-out
 *   reset. ⚠ Not called on the persist-failure arm: tearing the session down
 *   there takes the only copy of the successor seed with it.
 */
@Composable
fun RecoveryKitSection(
    sessionActorIdHex: String?,
    onSucceeded: (String) -> Unit,
    vm: RecoveryKitVM = hiltViewModel(),
) {
    val state by vm.state.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    // What makes this section's state stale is who is signed in AND which
    // actor's seat the client signs as: a succession's owed kit/sweep defers
    // while the client still holds the predecessor's (revoked) seat, so the
    // successor's seat arriving must re-fire the hydrate (apple's
    // `hydrateKey`, measured on iOS 2026-08-26).
    val connection by vm.connectionState.collectAsState()
    val seat = remember(connection) { vm.boundSeat() }

    LaunchedEffect(sessionActorIdHex, seat) { vm.hydrate(sessionActorIdHex) }
    LaunchedEffect(errorMessage) {
        errorMessage?.let {
            appMessages.showError(it)
            vm.consumeError()
        }
    }
    DisposableEffect(vm) {
        vm.onScreen = true
        vm.ceremonyHold.accountAppeared()
        onDispose {
            vm.onScreen = false
            // Leaving Account is also the edge a held-back supersession waits
            // for (`settings.md` § Recovery kit → *The persist-failure message
            // survives the page*): the user has had the whole visit to copy the
            // key, and the dead session under it may now go the ordinary way.
            vm.ceremonyHold.accountLeft()
            // The minted kit must not survive the view that displayed it —
            // leaving the Account page (or navigating deeper) drops it along
            // with the typed phrase and confirm token.
            vm.clearHeldSecrets()
        }
    }

    RecoveryKitSectionContent(
        state = state,
        onPhraseChange = vm::onPhraseChange,
        onCreate = { vm.createOrReplaceKit(usingHeldPhrase = false) },
        onReplace = { vm.createOrReplaceKit(usingHeldPhrase = true) },
        onLost = vm::requestSeedAloneReplacement,
        onEscrowReseal = vm::resealEscrowWithHeldKit,
        onVeto = vm::vetoPendingReplacement,
        onStolenConfirmChange = vm::onStolenConfirmChange,
        onStolen = { vm.succeedWithHeldKit(sessionActorIdHex, onSucceeded) },
        onSweepRetry = vm::retrySweep,
    )
}

/**
 * The section's stateless render surface — plain parameters and callbacks, so
 * it renders under Robolectric with no Hilt, no VM and no FFI (the two native
 * reads are injected: [pendingDays] and [qrMatrixOf]).
 *
 * **Every decision comes from shared Rust.** Enablement reads only the status
 * record's `allows_*` fields; the veto renders only while `pendingLandsAt` is
 * set and the escrow re-seal only when `allowsEscrowReseal` — never re-derived
 * from `status.kind`, which this function reads for the status line's copy
 * alone. With the status unread the four kit actions do not render — an
 * un-hydrated section must not offer ceremonies the account may not be able to
 * run — but the stolen trigger does ([RecoveryKitUiState.stolenVisible]): its
 * authorization is the kit, and the unread chain is exactly what a locked-out
 * owner cannot read. The sweep's lines are selected by the shared `sweepCopy`
 * ([sweepCopyOf]), never matched on the view's `kind` here.
 */
@Composable
fun RecoveryKitSectionContent(
    state: RecoveryKitUiState,
    onPhraseChange: (String) -> Unit,
    onCreate: () -> Unit,
    onReplace: () -> Unit,
    onLost: () -> Unit,
    onEscrowReseal: () -> Unit,
    onVeto: () -> Unit,
    onStolenConfirmChange: (String) -> Unit = {},
    onStolen: () -> Unit = {},
    onSweepRetry: () -> Unit = {},
    // The shared `sweep_copy` projection — android renders the retry button,
    // so it always declares `rendersRetry = true` (the degraded lines may then
    // name it). Injected FFI-free for the Robolectric harness.
    sweepCopyOf: (FfiSweepView) -> FfiSweepCopy = { view -> com.fauna.ffi.sweepCopy(view, true) },
    // `recovery_pending_days_remaining` — the shared rounding rule; only `now`
    // is this view's to own. Injected FFI-free for the Robolectric harness.
    pendingDays: (landsAt: Long) -> Long = { landsAt ->
        com.fauna.ffi.recoveryPendingDaysRemaining(landsAt, System.currentTimeMillis() / 1000)
            .toLong()
    },
    // The shared QR encoder; `null` on a refusal leaves the QR off rather than
    // crashing the page. Injected FFI-free for the same reason.
    qrMatrixOf: (String) -> QrMatrix? = { payload ->
        try {
            com.fauna.ffi.qrMatrix(payload)
        } catch (_: Exception) {
            null
        }
    },
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_KIT_SECTION)) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                stringResource(R.string.settings_recovery_kit_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(8.dp))

            state.sweepView?.let { view -> SweepLines(view, state.busy, sweepCopyOf, onSweepRetry) }

            Text(
                statusLine(state.status, pendingDays),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.RECOVERY_KIT_STATUS),
            )
            Spacer(Modifier.height(4.dp))
            Text(
                stringResource(R.string.settings_recovery_kit_desc),
                style = MaterialTheme.typography.bodySmall,
            )

            state.mintedSecretHex?.let { secret ->
                MintedKitDisplay(
                    secret = secret,
                    // The display is the bare hex (what a user writes on paper);
                    // copy and QR carry the account-naming URI, falling back to
                    // the bare secret the restore parser also accepts.
                    payload = state.mintedKitUri ?: secret,
                    escrowStored = state.mintedEscrowStored,
                    qrMatrixOf = qrMatrixOf,
                )
            }

            // The kit-in-hand entry — the onboarding `recovery_entry` screen's
            // own field reused inline (settings.md § Recovery kit → *Kit-in-hand
            // entry*), not a settings-scoped twin.
            if (state.phraseFieldVisible) {
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = state.phraseInput,
                    onValueChange = onPhraseChange,
                    label = { Text(stringResource(R.string.settings_recovery_kit_kit_phrase_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_ENTRY_PHRASE_FIELD),
                )
            }

            state.status?.let { status -> ActionButtons(status, state.busy, onCreate, onReplace, onLost, onEscrowReseal, onVeto) }

            if (state.stolenVisible) StolenAction(state, onStolenConfirmChange, onStolen)
        }
    }
}

/**
 * The post-succession group sweep's own lines (`settings.md` § Recovery kit →
 * *The sweep's own lines*): `recovery-kit-sweep-status` (what the sweep did)
 * and, its OWN element, never a qualifier on the first,
 * `recovery-kit-sweep-unvouched-status` (the roster it cannot vouch for). Each
 * is ABSENT, never present and empty, when the projection returns no line for
 * it — the silence on a succession with no groups at all is the projection's.
 *
 * The retry renders directly under them, gated on `owesWork` — deliberately NOT
 * on whether THIS device can finish the sweep: hiding it where the retry cannot
 * run would leave the degraded copy naming a control that is not on screen.
 * Every press answers in words on `error-message`.
 */
@Composable
private fun SweepLines(
    view: FfiSweepView,
    busy: Boolean,
    sweepCopyOf: (FfiSweepView) -> FfiSweepCopy,
    onSweepRetry: () -> Unit,
) {
    val copy = remember(view) { sweepCopyOf(view) }
    localized(copy.outcome)?.let {
        Text(
            it,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_SWEEP_STATUS),
        )
    }
    localized(copy.unattested)?.let {
        Text(
            it,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_SWEEP_UNVOUCHED_STATUS),
        )
    }
    if (view.owesWork) {
        OutlinedButton(
            onClick = onSweepRetry,
            enabled = !busy,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_SWEEP_RETRY_BUTTON),
        ) { Text(stringResource(R.string.settings_recovery_kit_sweep_retry)) }
    }
    Spacer(Modifier.height(4.dp))
}

/**
 * The succession trigger and its type-to-confirm gate — account deletion's
 * idiom, for the same reason: this is irreversible and re-points the whole
 * account, so a bare tap must not reach it. The warning rides as ID-less
 * chrome, as on every other app. The COMMIT gates on its wire kind; the confirm
 * field beside it stays typeable with no nest.
 */
@Composable
private fun StolenAction(
    state: RecoveryKitUiState,
    onStolenConfirmChange: (String) -> Unit,
    onStolen: () -> Unit,
) {
    val gate = faunaGate("fauna.recovery.succession.submit", enabled = state.stolenArmed)
    Spacer(Modifier.height(12.dp))
    Text(
        stringResource(R.string.settings_recovery_kit_stolen_warning),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.error,
    )
    OutlinedTextField(
        value = state.stolenConfirmInput,
        onValueChange = onStolenConfirmChange,
        label = { Text(stringResource(R.string.settings_recovery_kit_stolen_confirm_placeholder)) },
        singleLine = true,
        // The gate compares against a literal, so an auto-capitalized
        // "Succeed" would never arm the button and the user could not see why.
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrect = false),
        modifier = Modifier.fillMaxWidth().testTag(Ids.IDENTITY_STOLEN_CONFIRM_FIELD),
    )
    OutlinedButton(
        onClick = onStolen,
        enabled = gate.enabled,
        modifier = Modifier.testTag(Ids.IDENTITY_STOLEN_BUTTON),
    ) { Text(stringResource(R.string.settings_recovery_kit_stolen), color = MaterialTheme.colorScheme.error) }
    DisabledControlReasonText(gate.reason)
}

@Composable
private fun statusLine(status: FfiRecoveryKitStatus?, pendingDays: (Long) -> Long): String {
    // An un-hydrated (or unreadable) section paints `status_loading` rather
    // than nothing: it still owes the user a reason for the absent actions.
    if (status == null) return stringResource(R.string.settings_recovery_kit_status_loading)
    return when (status.kind) {
        "never-created" -> stringResource(R.string.settings_recovery_kit_status_never_created)
        "registered" -> stringResource(R.string.settings_recovery_kit_status_registered)
        "registered-no-escrow" -> stringResource(R.string.settings_recovery_kit_status_registered_no_escrow)
        "replacement-pending" -> stringResourceFmt(
            R.string.settings_recovery_kit_status_replacement_pending,
            status.pendingLandsAt?.let(pendingDays) ?: 0L,
        )
        // A kind this build does not know is a NEWER nest/app pairing, not a
        // corrupt read: say the honest thing rather than invent a state.
        else -> stringResource(R.string.settings_recovery_kit_status_loading)
    }
}

@Composable
private fun MintedKitDisplay(
    secret: String,
    payload: String,
    escrowStored: Boolean,
    qrMatrixOf: (String) -> QrMatrix?,
) {
    // Rendered through the onboarding screen's own ids — the same artifact,
    // shown the same way (priority #3). Deliberately NOT capture-suppressed: a
    // screenshot of a recovery kit is often the copy that saves the account
    // (`ScreenCapture.kt`, rule 1).
    Spacer(Modifier.height(8.dp))
    SelectionContainer {
        Text(
            secret,
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_SECRET_DISPLAY),
        )
    }
    CopyButton(
        testTag = Ids.RECOVERY_KIT_SECRET_COPY_BTN,
        text = payload,
        label = stringResource(R.string.common_copy),
    )
    val matrix = remember(payload) { qrMatrixOf(payload) }
    matrix?.let {
        Spacer(Modifier.height(8.dp))
        QrCanvas(matrix = it, modifier = Modifier.size(220.dp).testTag(Ids.RECOVERY_KIT_QR))
    }
    // ⚠ A failed escrow put is NOT an error: the registration landed, so the
    // secret above is live and the only copy in existence. A plain error here
    // would tell the user to discard the one thing they must write down.
    if (!escrowStored) {
        Text(
            stringResource(R.string.settings_recovery_kit_status_registered_no_escrow),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.tertiary,
        )
    }
}

@Composable
private fun ActionButtons(
    status: FfiRecoveryKitStatus,
    busy: Boolean,
    onCreate: () -> Unit,
    onReplace: () -> Unit,
    onLost: () -> Unit,
    onEscrowReseal: () -> Unit,
    onVeto: () -> Unit,
) {
    // Each commit gates on its own wire kind; the phrase field above stays
    // typeable with no nest, exactly as apple's `.faunaGate` placement.
    val createGate = faunaGate("fauna.recovery.registration.submit", enabled = status.allowsCreate && !busy)
    val replaceGate = faunaGate("fauna.recovery.registration.submit", enabled = status.allowsReplace && !busy)
    val lostGate = faunaGate("fauna.recovery.replacement.request", enabled = status.allowsLost && !busy)

    Spacer(Modifier.height(8.dp))
    OutlinedButton(
        onClick = onCreate,
        enabled = createGate.enabled,
        modifier = Modifier.testTag(Ids.RECOVERY_KIT_CREATE_BUTTON),
    ) { Text(stringResource(R.string.settings_recovery_kit_create)) }
    DisabledControlReasonText(createGate.reason)

    OutlinedButton(
        onClick = onReplace,
        enabled = replaceGate.enabled,
        modifier = Modifier.testTag(Ids.RECOVERY_KIT_REPLACE_BUTTON),
    ) { Text(stringResource(R.string.settings_recovery_kit_replace)) }
    DisabledControlReasonText(replaceGate.reason)

    OutlinedButton(
        onClick = onLost,
        enabled = lostGate.enabled,
        modifier = Modifier.testTag(Ids.RECOVERY_KIT_LOST_BUTTON),
    ) { Text(stringResource(R.string.settings_recovery_kit_lost)) }
    DisabledControlReasonText(lostGate.reason)

    // Renders only in the no-escrow state — the kit-in-hand re-put that
    // restores phrase recovery WITHOUT retiring the held kit.
    if (status.allowsEscrowReseal) {
        val resealGate = faunaGate("fauna.recovery.escrow.put", enabled = !busy)
        OutlinedButton(
            onClick = onEscrowReseal,
            enabled = resealGate.enabled,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_ESCROW_RESEAL_BUTTON),
        ) { Text(stringResource(R.string.settings_recovery_kit_escrow_reseal)) }
        DisabledControlReasonText(resealGate.reason)
    }

    // Renders only while a seed-alone replacement is in its window.
    if (status.pendingLandsAt != null) {
        val vetoGate = faunaGate("fauna.recovery.replacement.veto", enabled = !busy)
        OutlinedButton(
            onClick = onVeto,
            enabled = vetoGate.enabled,
            modifier = Modifier.testTag(Ids.RECOVERY_PENDING_VETO_BUTTON),
        ) { Text(stringResource(R.string.settings_recovery_kit_veto)) }
        DisabledControlReasonText(vetoGate.reason)
    }
}
