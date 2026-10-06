package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.navigation.NavController
import com.fauna.app.R
import android.app.Activity
import androidx.compose.ui.platform.LocalContext
import com.fauna.app.age.findActivity
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.util.localizedNested
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.onboarding.InviteRequestState
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.OobCodeState
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * How often this page re-polls while it shows `PendingReview`.
 *
 * Reads the shared constant — onboarding.md § The pending-invite surface,
 * "read by all 7 apps — never seven hand-copied numbers".
 */
private val INVITE_POLL_INTERVAL_MS = com.fauna.ffi.onboarding.inviteRecheckPollMs().toLong()

/**
 * Snapshot-driven view of OnboardingMachine.invite_request_snapshot()
 * per docs/goal/behavior/onboarding.md §3 "Invite request". Two independent rows
 * — admin-flow submit/recheck and out-of-band code verify — both feed
 * into a single Continue button that routes by snapshot state.
 */
@HiltViewModel
class InviteRequestVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    /**
     * The store-age round rides every admission call (the submit here, the
     * redeem in [continueAction]) — `OnboardingHost.attachStoreAgeClaim`; the
     * Activity is what Play's own consent prompt needs.
     */
    fun submit(activity: Activity?) = viewModelScope.launch {
        host.attachStoreAgeClaim(activity)
        host.machine.wizardSubmitInviteRequest()
        // "The only write moment" (onboarding.md § 3 Persistence callouts) —
        // this journey has no wizard exit to hang the write on any more.
        host.persistPendingInviteSlot()
    }

    fun recheck() = viewModelScope.launch { host.machine.recheckInviteStatus() }

    /**
     * One single-shot poll of the pending request.
     *
     * Unlike [recheck] this must also ROUTE an approval: `recheckInviteStatus`
     * resolves a confirmed admission into `WizardOutcome::LoggedIn` by itself
     * (the registered-probe), and nothing else would carry the user into the
     * app — that is the whole point of the surface. Returns the outcome when the
     * wizard finished, else null.
     */
    suspend fun poll(): WizardOutcome? = runCatching {
        val step = host.machine.recheckInviteStatus()
        host.handleWizardExit(step)
    }.getOrElse {
        // A poll that cannot reach the nest must not wedge the loop or paint an
        // error over a page that is simply waiting; the next tick retries.
        null
    }
    fun verifyCode(code: String) = viewModelScope.launch { host.machine.verifyOobInviteCode(code) }
    fun cancel() { host.machine.cancelInviteOp() }

    suspend fun continueAction(activity: Activity?): OnboardingStep {
        val snap = host.machine.inviteRequestSnapshot()
        // Continue is the out-of-band code's redeem and nothing else
        // (onboarding.md § 3 — the button's row). The Approved and PendingReview
        // branches retired 2026-08-12 with the continue-exit: no live nest serves
        // Approved, and the pending-review journey advances by polling.
        val oobValid = snap.outOfBandCodeState is OobCodeState.Valid
        return when {
            oobValid -> {
                host.attachStoreAgeClaim(activity)
                host.machine.redeemInvite()
            }
            else -> host.machine.step()  // refuse — Continue is disabled otherwise
        }
    }
}

@Composable
fun InviteRequestScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: InviteRequestVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.host.machine.inviteRequestSnapshot() }
    val isLoading = remember(tick) { vm.host.machine.isLoading() }
    val coroutineScope = rememberCoroutineScope()
    // For the store-age round's Play consent prompt (OnboardingHost.attachStoreAgeClaim).
    val activity = LocalContext.current.findActivity()

    var codeInput by remember { mutableStateOf("") }

    // The pending-invite poll. Approval reaches an *unregistered* actor through
    // no push channel — every notification plane is keyed on a bearer-proven
    // actor_id the requester does not have yet — so the client asks
    // (onboarding.md § The pending-invite surface: "Poll is the channel —
    // structurally, not provisionally").
    //
    // Keyed on the PendingReview state so the same-session wait and the relaunch
    // hydration are one code path, and cancelled with the composable, so leaving
    // the surface stops the polling.
    val pendingReview = snap.state is InviteRequestState.PendingReview
    LaunchedEffect(pendingReview) {
        if (!pendingReview) return@LaunchedEffect
        // First poll fires immediately — that is what makes the relaunch case
        // (a hydrated PendingReview) resolve without a full interval's stare.
        vm.poll()?.let(onWizardExit)
        while (true) {
            delay(INVITE_POLL_INTERVAL_MS)
            vm.poll()?.let(onWizardExit)
        }
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_invite_request_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_invite_request_subtitle),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // The store-age round's outcome, before submit/redeem — derived in the
        // shared machine from the claim `OnboardingHost.attachStoreAgeClaim`
        // hands `set_age_claim`; `null` = the store shared nothing (a `foss`
        // build included), so nothing paints (family-safety.md § App surface →
        // *Age-band surfaces*; a `platform_elements` entry for android/ios).
        // Its args are nested keys, hence the nested resolve.
        localizedNested(snap.ageNotice)?.let { notice ->
            Spacer(Modifier.height(8.dp))
            Text(
                notice,
                modifier = Modifier.fillMaxWidth().testTag(Ids.INVITE_REQUEST_AGE_NOTICE),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Spacer(Modifier.height(24.dp))

        // ── Admin-flow row ──────────────────────────────────────────────
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(
                onClick = { vm.submit(activity) },
                enabled = !isLoading && snap.state !is InviteRequestState.Submitting,
                modifier = Modifier.testTag(Ids.INVITE_REQUEST_SUBMIT_BUTTON),
            ) {
                Text(stringResource(R.string.onboarding_invite_request_submit))
            }
            if (snap.recheckVisible) {
                OutlinedButton(
                    onClick = { vm.recheck() },
                    enabled = !isLoading,
                    modifier = Modifier.testTag(Ids.INVITE_REQUEST_RECHECK_BUTTON),
                ) {
                    Text(stringResource(R.string.onboarding_invite_recheck_button))
                }
            }
        }

        Spacer(Modifier.height(8.dp))
        // invite-request-status: localized message text from snapshot.message,
        // with fallback to the per-state default key.
        val adminStatusText = localizedOnboardingText(snap.message)
            ?: defaultStateLabel(snap.state)
        if (adminStatusText != null) {
            Text(
                adminStatusText,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.INVITE_REQUEST_STATUS),
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        Spacer(Modifier.height(32.dp))

        // ── Out-of-band code row ────────────────────────────────────────
        Text(
            stringResource(R.string.onboarding_invite_request_code_section_title),
            style = MaterialTheme.typography.titleSmall,
        )
        Spacer(Modifier.height(8.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = codeInput,
                onValueChange = { codeInput = it },
                modifier = Modifier.weight(1f).testTag(Ids.INVITE_CODE_INPUT),
                singleLine = true,
                enabled = !isLoading,
                placeholder = {
                    Text(stringResource(R.string.onboarding_invite_request_code_placeholder))
                },
            )
            Button(
                onClick = { vm.verifyCode(codeInput) },
                enabled = codeInput.isNotBlank() && !isLoading,
                modifier = Modifier.testTag(Ids.INVITE_CODE_CHECK_BUTTON),
            ) { Text(stringResource(R.string.common_check)) }
        }
        Spacer(Modifier.height(8.dp))
        val oobStatusText = localizedOnboardingText(snap.oobMessage)
        if (oobStatusText != null) {
            Text(
                oobStatusText,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.INVITE_CODE_STATUS),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        // "This account will be supervised by X" — rendered BEFORE redemption
        // when the checked code carries a supervised designation
        // (family-safety.md § Wire & data shape / § App surface).
        val supervisedByGuardian = (snap.outOfBandCodeState as? OobCodeState.Valid)?.supervisedBy
        if (supervisedByGuardian != null) {
            Text(
                stringResourceFmt(R.string.family_supervised_notice_onboarding, supervisedByGuardian),
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.INVITE_CODE_SUPERVISED_NOTICE),
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        Spacer(Modifier.weight(1f))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = {
                    vm.cancel()
                    navController.popBackStack()
                },
                modifier = Modifier.testTag(Ids.INVITE_REQUEST_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Button(
                onClick = {
                    coroutineScope.launch {
                        val step = vm.continueAction(activity)
                        if (step == OnboardingStep.DONE) {
                            vm.host.handleWizardExit(step)?.let(onWizardExit)
                        } else {
                            navController.navigate(routeForStep(step))
                        }
                    }
                },
                enabled = snap.continueEnabled && !isLoading,
                modifier = Modifier.testTag(Ids.INVITE_REQUEST_CONTINUE_BUTTON),
            ) { Text(stringResource(R.string.common_continue)) }
        }
    }
}

/** Default label per InviteRequestState when the snapshot's message key is empty. */
@Composable
private fun defaultStateLabel(state: InviteRequestState): String? = when (state) {
    InviteRequestState.Idle -> stringResource(R.string.onboarding_invite_idle)
    InviteRequestState.Submitting -> stringResource(R.string.onboarding_invite_submitting)
    InviteRequestState.Rechecking -> stringResource(R.string.onboarding_invite_rechecking)
    is InviteRequestState.PendingReview -> stringResource(R.string.onboarding_invite_pending_review)
    // (the `Approved` arm retired 2026-08-12 with the variant — it rendered
    // literal "?" placeholders for the quota it could not format anyway)
    is InviteRequestState.Denied -> stringResourceFmt(R.string.onboarding_invite_denied, state.reason)
    is InviteRequestState.Error -> state.cause
}

