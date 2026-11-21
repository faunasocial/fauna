package com.fauna.app.ui.screen

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.components.AccountSwitcherRows
import com.fauna.app.ui.components.OnboardingErrorText
import com.fauna.ffi.FfiAccountEntry
import social.fauna.generated.Ids
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.OutlinedButton

/**
 * Launch-screen retry surface per docs/goal/behavior/onboarding.md
 * §"App-launch routing": shown when the silent-challenge fast path
 * couldn't reach the saved nest (IOException, HTTP 5xx, DNS failure,
 * etc.). Out of the seven-page wizard count — rendered by the launch
 * flow before the wizard mounts.
 *
 * Element IDs match the cross-app convention established by Web
 * and adopted by Windows — `launch-transient-error`,
 * `launch-retry-button`, `launch-fallthrough-button`, plus the optional
 * `launch-recover-button` (surviving-device recovery entry, shown only when
 * the device knows of ≥1 custodied box; per the new
 * `launch_retry` page block in `tests/e2e-unified/ui.yaml`).
 */
@Composable
fun LaunchRetryScreen(
    /** `launch-identity-label`: the bound account's cached identity
     *  (`AppLaunchVM.launchIdentityLine`); null → absent. */
    identityLine: String? = null,
    onRetry: () -> Unit,
    onUseDifferentNest: () -> Unit,
    /** The custodied boxes the device knows of; `launch-recover-button` shows iff non-empty. */
    recoverableBoxes: List<String> = emptyList(),
    onRecover: () -> Unit = {},
    /** `launch-retire-button`: always shown — it needs only a cloud token, no
     *  custodied seed (`nest-retirement.md` § Layout & flow). */
    onRetire: () -> Unit = {},
    /** The device's accounts in registry order; the account switcher
     *  (`account-switcher-list`) paints iff more than one. */
    accounts: List<FfiAccountEntry> = emptyList(),
    /** The active (unreachable) account — its row is plain, never a switch. */
    activeActorId: String? = null,
    accountLabel: (FfiAccountEntry) -> String = { it.actorId },
    /** A tap on a non-active row — the caller runs the Settings switcher's
     *  gated switch and re-runs the launch as that account. */
    onSwitch: (actorId: String) -> Unit = {},
    /** A refused switch, painted as `error-message`; the surface stays. */
    switchError: String? = null,
) {
    // Centered as before; scrollable once the switcher's rows outgrow the screen.
    Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            identityLine?.let {
                LaunchIdentityLabel(it)
                Spacer(Modifier.height(16.dp))
            }
            Text(
                stringResource(R.string.launch_retry_title),
                style = MaterialTheme.typography.headlineSmall,
                textAlign = TextAlign.Center,
                modifier = Modifier.testTag(Ids.LAUNCH_TRANSIENT_ERROR),
            )
            Spacer(Modifier.height(32.dp))
            Button(
                onClick = onRetry,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.LAUNCH_RETRY_BUTTON),
            ) { Text(stringResource(R.string.launch_retry_button)) }
            if (recoverableBoxes.isNotEmpty()) {
                Spacer(Modifier.height(8.dp))
                OutlinedButton(
                    onClick = onRecover,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(Ids.LAUNCH_RECOVER_BUTTON),
                ) { Text(stringResource(R.string.onboarding_launch_recover_lost_box)) }
            }
            // The kids build never routes into nest retirement (a child joins a
            // guardian's nest by invite; dynamic-features.md, the kids spine).
            if (!BuildConfig.KIDS) {
                Spacer(Modifier.height(8.dp))
                OutlinedButton(
                    onClick = onRetire,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(Ids.LAUNCH_RETIRE_BUTTON),
                ) { Text(stringResource(R.string.onboarding_launch_retire_server)) }
            }
            Spacer(Modifier.height(8.dp))
            OutlinedButton(
                onClick = onUseDifferentNest,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.LAUNCH_FALLTHROUGH_BUTTON),
            ) { Text(stringResource(R.string.launch_use_different_nest)) }
            // The way back to the previous account after a failed append-mode
            // finish (`onboarding.md` § Wizard exit handling → *The finished
            // wizard holds*): the same rows the Settings switcher paints, with
            // none of its per-row extras. Nothing with one account (or none).
            if (accounts.size > 1) {
                Spacer(Modifier.height(24.dp))
                Column(
                    modifier = Modifier.fillMaxWidth().testTag(Ids.ACCOUNT_SWITCHER_LIST),
                ) {
                    Text(
                        stringResource(R.string.settings_account_page_accounts),
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Spacer(Modifier.height(8.dp))
                    AccountSwitcherRows(
                        accounts = accounts,
                        activeActorId = activeActorId,
                        label = accountLabel,
                        onSwitch = onSwitch,
                    )
                }
            }
            OnboardingErrorText(switchError)
        }
    }
}

/**
 * `launch_progress` — the in-flight launch: the "Signing you in…" line (tui's
 * and windows' `launch.signing_in`) and `launch-identity-label`, the bound
 * account's cached `handle@domain · tier` (`onboarding.md` § App-launch routing
 * → *The cached identity on the launch surfaces*), absent while the cache holds
 * no handle. Never nothing: from the wizard's exit to the app this is what the
 * activity shows (`onboarding.md` § Wizard exit handling → *The wizard becomes
 * the app in place*), and a first sign-in has no cached handle.
 */
@Composable
fun LaunchProgressScreen(identityLine: String?) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.launch_signing_in),
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
        )
        if (identityLine != null) LaunchIdentityLabel(identityLine)
    }
}

/** The bare identity line carrying `launch-identity-label`. */
@Composable
private fun LaunchIdentityLabel(line: String) {
    Text(
        line,
        style = MaterialTheme.typography.bodyMedium,
        textAlign = TextAlign.Center,
        modifier = Modifier.testTag(Ids.LAUNCH_IDENTITY_LABEL),
    )
}
