package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
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
import com.fauna.app.core.AccountStores
import com.fauna.app.core.HexUtil
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.ShellLog
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.shortNestId
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Box-selection hub for total-box-loss recovery
 * (docs/goal/architecture/nest/box-recovery.md § Recovery UI (step 4), Task E).
 * The box list is the account plane's custody map
 * (`box-recovery.md` § The plane-era recovery floor, *(b) The reads*): this
 * device's own account store joined with a cold read from the stored nest when
 * one resolves and answers — never either-or, since a surviving device's stored
 * nest is, in the case recovery exists for, the dead box. The reachable-nest arm
 * ([loadReachableBoxesIfNestUrl]) calls the shared `deploymentSeeds()` getter,
 * which does the join itself, and falls back to the local read when the nest
 * cannot be reached; the offline arm ([loadOfflineBoxesIfNoNestUrl]) reads the
 * local store alone. Both self-guard on `nestUrl()` presence, so calling both
 * unconditionally on entry is safe — exactly one runs. In production the list
 * starts empty until one of those two arms populates it; the e2e also injects
 * directly via `set_recovery_boxes`.
 */
@HiltViewModel
class NestRecoveryVM @Inject constructor(
    val host: OnboardingHost,
    // The account-store container both reads open — the same accessor
    // `startAccountRuntime` and both erases use (the platform default is the
    // wrong place inside the android sandbox).
    private val accountStores: AccountStores,
) : ViewModel() {
    fun selectBox(nestActorId: String) = host.machine.selectRecoveryBox(nestActorId)

    /**
     * Reachable-nest read: with a stored nest URL, connect a short-lived
     * [com.fauna.ffi.FfiNestClient] to it and read the custodied box list via
     * [com.fauna.ffi.deploymentSeeds] — this device's own store joined with a
     * cold read from that nest, in one shared call. When the nest cannot be
     * reached (the total-box-loss case where the saved nest *is* the dead box)
     * it reads the local store alone, so a dead stored nest never hides what
     * this device holds. Best-effort — never surfaces a page error. No-ops when
     * there's no nest URL (the offline arm owns that case), no identity yet, or
     * the machine already holds a non-empty list (never clobber a list already
     * on screen). `disconnect()`s afterward — this is a one-off probe, not the
     * authenticated session `ApiClient` builds.
     */
    fun loadReachableBoxesIfNestUrl() {
        if (host.machine.recoveryBoxes().isNotEmpty()) return
        val nestUrl = host.machine.nestUrl()
        if (nestUrl.isEmpty()) return
        val secretHex = host.machine.effectiveSecret() ?: return
        viewModelScope.launch {
            val ownerSecret = try {
                HexUtil.hexToBytes(secretHex)
            } catch (e: Exception) {
                ShellLog.w("NestRecoveryVM", "loadReachableBoxesIfNestUrl: bad secret: ${e.message}")
                return@launch
            }
            val client = try {
                com.fauna.ffi.FfiNestClient(nestUrl, ownerSecret)
            } catch (e: Exception) {
                ShellLog.w("NestRecoveryVM", "loadReachableBoxesIfNestUrl: client build failed: ${e.message}")
                return@launch
            }
            val storeContainerDir = accountStores.accountStoreContainerDir()
            try {
                val boxes = try {
                    client.connect()
                    com.fauna.ffi.deploymentSeeds(client, ownerSecret, storeContainerDir)
                } catch (e: Exception) {
                    ShellLog.w(
                        "NestRecoveryVM",
                        "loadReachableBoxesIfNestUrl: nest read failed, reading this device's store alone: ${e.message}",
                    )
                    com.fauna.ffi.deploymentSeedsLocal(
                        ownerSecret, "", storeContainerDir,
                    )
                }.map { it.nestActorId }
                if (boxes.isNotEmpty() && host.machine.recoveryBoxes().isEmpty()) {
                    host.machine.setRecoveryBoxes(boxes)
                }
            } catch (e: Exception) {
                ShellLog.w("NestRecoveryVM", "loadReachableBoxesIfNestUrl failed (best-effort): ${e.message}")
            } finally {
                client.disconnect()
            }
        }
    }

    /**
     * The single-last-box total-loss case, where no
     * nest URL resolves. Reads this device's own account store
     * ([com.fauna.ffi.deploymentSeedsLocal]) — the rows the custody leg merged
     * while the device was online. No-ops when a nest URL IS resolved (the
     * reachable arm owns that case), when there's no identity yet, or when the
     * machine already holds a non-empty list (never clobber a list already on
     * screen — the e2e's `set_recovery_boxes` injection, or a prior successful
     * read). Best-effort: a store that does not exist reads as an empty list,
     * a read fault is logged.
     */
    fun loadOfflineBoxesIfNoNestUrl() {
        if (host.machine.recoveryBoxes().isNotEmpty()) return
        if (host.machine.nestUrl().isNotEmpty()) return
        val secretHex = host.machine.effectiveSecret() ?: return
        try {
            val ownerSecret = HexUtil.hexToBytes(secretHex)
            // `appDataDir` is unused by the plane read (it rooted the retired
            // `__config` replica) — the store is located by its container.
            val boxes = com.fauna.ffi.deploymentSeedsLocal(
                ownerSecret, "", accountStores.accountStoreContainerDir(),
            ).map { it.nestActorId }
            if (boxes.isNotEmpty()) {
                host.machine.setRecoveryBoxes(boxes)
            }
        } catch (e: Exception) {
            ShellLog.w("NestRecoveryVM", "loadOfflineBoxesIfNoNestUrl failed: ${e.message}")
        }
    }

    /** Buttons are gated on a selection, so a thrown InvalidTransition here is defense-in-depth only. */
    fun recoverViaCloud(): OnboardingStep {
        try {
            host.machine.recoverViaCloud()
        } catch (e: Exception) {
            // surfaced via host.machine.errorMessage(), rendered as error-message
        }
        return host.machine.step()
    }

    fun recoverViaSelfhosted(): OnboardingStep {
        try {
            host.machine.recoverViaSelfhosted()
        } catch (e: Exception) {
            // surfaced via host.machine.errorMessage(), rendered as error-message
        }
        return host.machine.step()
    }

    fun back() = host.machine.back()
}

@Composable
fun NestRecoveryScreen(
    navController: NavController,
    vm: NestRecoveryVM = hiltViewModel(),
) {
    LaunchedEffect(Unit) {
        vm.loadReachableBoxesIfNestUrl()
        vm.loadOfflineBoxesIfNoNestUrl()
    }

    val tick by vm.host.tick.collectAsState()
    val boxes = remember(tick) { vm.host.machine.recoveryBoxes() }
    val selectedId = remember(tick) { vm.host.machine.recoverySelectedNestId() }
    val errorMessage = remember(tick) { vm.host.machine.errorMessage() }
    val hasSelection = selectedId != null

    Column(modifier = Modifier.fillMaxSize().padding(24.dp)) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_recovery_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_recovery_subtitle),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        if (boxes.isEmpty()) {
            Text(
                stringResource(R.string.onboarding_recovery_empty_message),
                modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_BOX_EMPTY_MESSAGE),
                style = MaterialTheme.typography.bodyMedium,
            )
        } else {
            Text(
                stringResource(R.string.onboarding_recovery_box_list_label),
                style = MaterialTheme.typography.titleSmall,
            )
            Spacer(Modifier.height(8.dp))
            Column(modifier = Modifier.testTag(Ids.RECOVER_BOX_LIST)) {
                boxes.forEachIndexed { idx, nestActorId ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = selectedId == nestActorId,
                                onClick = { vm.selectBox(nestActorId) },
                            )
                            .testTag("recover-box-item-$idx"),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = selectedId == nestActorId,
                            onClick = { vm.selectBox(nestActorId) },
                        )
                        Column(Modifier.padding(start = 8.dp)) {
                            Text(
                                shortNestId(nestActorId),
                                style = MaterialTheme.typography.bodyMedium,
                            )
                            Text(
                                stringResource(R.string.onboarding_recovery_box_item_hint),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        }

        if (!errorMessage.isNullOrEmpty()) {
            Spacer(Modifier.height(8.dp))
            Text(
                errorMessage,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ERROR_MESSAGE),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }

        Spacer(Modifier.weight(1f))

        Button(
            onClick = { navController.navigate(routeForStep(vm.recoverViaCloud())) },
            enabled = hasSelection,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_METHOD_CLOUD_BUTTON),
        ) { Text(stringResource(R.string.onboarding_recovery_method_cloud)) }

        Spacer(Modifier.height(8.dp))

        OutlinedButton(
            onClick = { navController.navigate(routeForStep(vm.recoverViaSelfhosted())) },
            enabled = hasSelection,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_METHOD_SELFHOSTED_BUTTON),
        ) { Text(stringResource(R.string.onboarding_recovery_method_selfhosted)) }

        Spacer(Modifier.height(8.dp))

        OutlinedButton(
            onClick = {
                vm.back()
                navController.popBackStack()
            },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_BACK_BUTTON),
        ) { Text(stringResource(R.string.common_back)) }
    }
}
