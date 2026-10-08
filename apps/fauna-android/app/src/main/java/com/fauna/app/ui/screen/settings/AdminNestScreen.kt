package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.AdminNestVM
import com.fauna.ffi.FfiAdminRegionView
import com.fauna.ffi.FfiIssuerForcedArm
import com.fauna.ffi.FfiIssuerKeyRow
import com.fauna.ffi.FfiIssuerKeyView
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.NodeMode
import social.fauna.generated.Ids

/**
 * The admin `admin-nest` page (admin.md § N Nest): the home for nest-wide admin
 * settings that aren't a feature page, introduced by the per-page-services
 * redesign (2026-06-04, admin.md § Admin IA redesign — which removed the
 * standalone `admin-services` page). It carries:
 *
 *  - the admin `admin-service-pairing-toggle` / `-status` (the one live
 *    service flag that gates `fauna.pair.add`; the user-facing link/unlink
 *    surface lives on `nests` — moved off the removed Services page),
 *  - the admin-set client-facing API `admin-nest-serving-port-*` field
 *    (`fauna.admin.set_serving_port`; nest/common.md § Serving ports),
 *  - the `admin-nest-nat-mode-*` control (`fauna.setup.nat_mode` via the
 *    shared `AdminNatModeMachine` — the post-onboarding change surface for
 *    the NAT axis the wizard's `nat_mode_choice` page confirms once at claim),
 *  - the read-only host-OS-maintenance row (`nest-os-*`, installers/vps.md
 *    § Host OS Maintenance § 4 — status line + count badge + restart-now),
 *  - the outside-app sign-in key section (`admin-nest-oauth-*`,
 *    authorization-server.md § The issuer → Two rotation arms) directly after
 *    the deployment-identity rotation, and
 *  - the `admin-factory-reset-*` danger zone (moved off Settings).
 *
 * Stateless [AdminNestContent] is split out for the Compose test harness; the
 * VM-bound [AdminNestScreen] is the wrapper the NavHost mounts. Dumb renderer
 * over [AdminNestVM] — no nest logic in the shell (priority #2). Mirrors the
 * Linux lead (`apps/fauna-linux/src/views/admin.rs` `build_nest_page`).
 *
 * The android admin pages are NavHost routes (admin.md § Navigation model lists
 * the cross-app shell-unification as a separate gap); `admin-nav-back` here
 * pops back to the admin dashboard, matching the sibling admin pages.
 */
@Composable
fun AdminNestScreen(
    navController: NavController,
    onFactoryResetComplete: () -> Unit,
    vm: AdminNestVM = hiltViewModel(),
) {
    val pairing by vm.pairing.collectAsState()
    val servingPort by vm.servingPort.collectAsState()
    val frontedByRouter by vm.frontedByRouter.collectAsState()
    val osSecurityUpdates by vm.osSecurityUpdates.collectAsState()
    val osRebootPending by vm.osRebootPending.collectAsState()
    val restartingNow by vm.restartingNow.collectAsState()
    val working by vm.working.collectAsState()
    val error by vm.error.collectAsState()
    val natSelectedMode by vm.natSelectedMode.collectAsState()
    val natMessage by vm.natMessage.collectAsState()
    val natSubmitEnabled by vm.natSubmitEnabled.collectAsState()
    val natSubmitting by vm.natSubmitting.collectAsState()
    val regionView by vm.regionView.collectAsState()
    val regionWorking by vm.regionWorking.collectAsState()
    val seedRotateConfirm by vm.seedRotateConfirm.collectAsState()
    val seedRotateStatus by vm.seedRotateStatus.collectAsState()
    val takedownArmed by vm.takedownArmed.collectAsState()
    val takedownStatus by vm.takedownStatus.collectAsState()
    val reports by vm.reports.collectAsState()
    val takedownPrefill by vm.takedownPrefill.collectAsState()
    // ONE flow for the whole sign-in key section, so a dispatch's verdict,
    // re-read key set and released in-flight guard recompose together.
    val oauth by vm.oauth.collectAsState()

    val invalidServingPortMessage = stringResource(R.string.admin_nest_page_serving_port_invalid)
    val invalidRegionMessage = stringResource(R.string.admin_nest_page_region_invalid)

    AdminNestContent(
        pairing = pairing,
        servingPort = servingPort,
        frontedByRouter = frontedByRouter,
        osSecurityUpdates = osSecurityUpdates,
        osRebootPending = osRebootPending,
        restartingNow = restartingNow,
        working = working,
        error = error,
        natSelectedMode = natSelectedMode,
        natMessage = natMessage,
        natSubmitEnabled = natSubmitEnabled,
        natSubmitting = natSubmitting,
        regionView = regionView,
        regionWorking = regionWorking,
        seedRotateConfirm = seedRotateConfirm,
        seedRotateStatus = seedRotateStatus,
        takedownArmed = takedownArmed,
        takedownStatus = takedownStatus,
        oauthKeys = oauth.keys,
        oauthArmed = oauth.armed,
        oauthStatus = oauth.status,
        oauthInFlight = oauth.inFlight,
        onBack = { navController.popBackStack() },
        onSetPairing = vm::setPairing,
        onSaveServingPort = vm::setServingPort,
        onInvalidServingPort = { vm.reportInvalidServingPort(invalidServingPortMessage) },
        onRestartNow = vm::restartNow,
        onSelectNatMode = vm::selectNatMode,
        onSaveNatMode = vm::saveNatMode,
        onSaveRegion = vm::setRegion,
        onWithdrawRegion = { vm.setRegion(null) },
        onInvalidRegion = { vm.reportInvalidRegion(invalidRegionMessage) },
        onArmSeedRotate = vm::armSeedRotate,
        onCancelSeedRotate = vm::cancelSeedRotate,
        onConfirmSeedRotate = vm::confirmSeedRotate,
        onArmTakedown = vm::armTakedown,
        onCancelTakedown = vm::cancelTakedown,
        onConfirmTakedown = vm::confirmTakedown,
        onRotateIssuerKey = vm::rotateIssuerKey,
        onArmOauthForced = vm::armOauthForced,
        onCancelOauthForced = vm::cancelOauthForced,
        onConfirmOauthForced = vm::confirmOauthForced,
        onFactoryReset = { vm.factoryReset(onFactoryResetComplete) },
        reports = reports,
        takedownPrefill = takedownPrefill,
        onResolveReport = vm::resolveReport,
        onOpenReportTakedown = vm::openReportTakedown,
        onConsumeTakedownPrefill = vm::consumeTakedownPrefill,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminNestContent(
    pairing: Boolean,
    servingPort: Int,
    frontedByRouter: Boolean,
    osSecurityUpdates: Int,
    osRebootPending: Boolean,
    restartingNow: Boolean,
    working: Boolean,
    error: String?,
    natSelectedMode: NodeMode,
    natMessage: LocalizedText?,
    natSubmitEnabled: Boolean,
    natSubmitting: Boolean,
    regionView: FfiAdminRegionView?,
    regionWorking: Boolean,
    seedRotateConfirm: AdminNestVM.SeedRotateConfirmState?,
    seedRotateStatus: String?,
    takedownArmed: AdminNestVM.TakedownArmed?,
    takedownStatus: String?,
    oauthKeys: AdminNestVM.OauthKeysRead,
    oauthArmed: AdminNestVM.OauthArmed?,
    oauthStatus: String?,
    oauthInFlight: Boolean,
    onBack: () -> Unit,
    onSetPairing: (Boolean) -> Unit,
    onSaveServingPort: (Int) -> Unit,
    onInvalidServingPort: () -> Unit,
    onRestartNow: () -> Unit,
    onSelectNatMode: (NodeMode) -> Unit,
    onSaveNatMode: () -> Unit,
    onSaveRegion: (String) -> Unit,
    onWithdrawRegion: () -> Unit,
    onInvalidRegion: () -> Unit,
    onArmSeedRotate: () -> Unit,
    onCancelSeedRotate: () -> Unit,
    onConfirmSeedRotate: () -> Unit,
    onArmTakedown: (String, Boolean, String, Boolean, com.fauna.ffi.FfiTakedownFormView) -> Unit,
    onCancelTakedown: () -> Unit,
    onConfirmTakedown: () -> Unit,
    onRotateIssuerKey: () -> Unit,
    onArmOauthForced: (FfiIssuerForcedArm) -> Unit,
    onCancelOauthForced: () -> Unit,
    onConfirmOauthForced: (FfiIssuerForcedArm) -> Unit,
    onFactoryReset: () -> Unit,
    // The shared `parse_port` validator (u16 in [1, 65535], reject 0), injected so
    // the Robolectric content-test can pass an FFI-free stub (mirrors AttendeeRow's
    // `view`; symmetric with AdminCalendarContent's CaldavPortField).
    parsePort: (String) -> Int? = { com.fauna.ffi.parsePort(it)?.toInt() },
    // The shared `admin_parse_region_code` validator (declared, never
    // detected — no case-fold), injected for the same FFI-free-test reason.
    parseRegionCode: (String) -> String? = {
        runCatching { com.fauna.ffi.adminParseRegionCode(it) }.getOrNull()
    },
    // The shared `takedown_form_view` fold (moderation.md § Legal takedown →
    // Invocation surface), injected for the same FFI-free-test reason.
    takedownFormView: (String, Boolean, String, Boolean) -> com.fauna.ffi.FfiTakedownFormView =
        { id, conversation, reference, restore ->
            com.fauna.ffi.takedownFormView(id, conversation, reference, restore)
        },
    // The shared `issuer_key_row_label` / `issuer_key_rotate_cost` folds
    // (authorization-server.md § The issuer), injected for the same FFI-free-
    // test reason. The row label's second argument is the wall clock at paint,
    // in epoch seconds — a retired key's countdown is the point of its line.
    issuerKeyRowLabel: (FfiIssuerKeyRow, Long) -> LocalizedText =
        { row, nowSecs -> com.fauna.ffi.issuerKeyRowLabel(row, nowSecs) },
    issuerKeyRotateCost: (FfiIssuerKeyView) -> LocalizedText =
        { view -> com.fauna.ffi.issuerKeyRotateCost(view) },
    // The open abuse reports (`admin-nest-reports-section`; moderation.md §
    // User-initiated reporting → *Where it lands*) and the takedown console's
    // one-shot prefill from a row's *open takedown*. Defaults keep the existing
    // Robolectric harnesses unchanged.
    reports: AdminNestVM.ReportsState = AdminNestVM.ReportsState(),
    takedownPrefill: com.fauna.ffi.FfiTakedownPrefill? = null,
    onResolveReport: (com.fauna.ffi.FfiReportQueueRow, Boolean) -> Unit = { _, _ -> },
    onOpenReportTakedown: (com.fauna.ffi.FfiReportQueueRow) -> Unit = {},
    onConsumeTakedownPrefill: () -> Unit = {},
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_nest_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── Admin pairing toggle (fauna.admin.services.update name="pairing") ──
            // The nest-level master switch for user-initiated nest pairing
            // (per-user multi-homing). Default on; off → the nest rejects
            // fauna.pair.add. Moved off the removed Services page.
            PairingRow(
                checked = pairing,
                working = working,
                onCheckedChange = onSetPairing,
            )

            // ── Serving port (fauna.admin.set_serving_port) ──
            // The admin-set client-facing API serving port (nest/common.md §
            // Serving ports), the symmetric twin of the CalDAV port on
            // admin-calendar. A text_input + save button (no shared policy
            // machine): seeded from setup.status, written via the raw
            // fauna.admin.set_serving_port kind. Governs only the router-less
            // direct listener; inert behind the :443 SNI router.
            ServingPortField(
                servingPort = servingPort,
                frontedByRouter = frontedByRouter,
                working = working,
                onSavePort = onSaveServingPort,
                onInvalidPort = onInvalidServingPort,
                parsePort = parsePort,
            )

            // ── NAT-mode control (fauna.setup.nat_mode via the shared AdminNatModeMachine) ──
            // The post-onboarding change surface for the axis the wizard's
            // nat_mode_choice page confirmed once at claim (admin.md § Nest →
            // NAT-mode control). Radios + save + status off the shared machine;
            // radio labels reuse the onboarding.nat_mode strings so both surfaces
            // read identically. No defer button — navigating away is the defer.
            // Save stays enabled after success (mutable upsert; an immediate
            // re-flip is allowed).
            NatModeSection(
                selectedMode = natSelectedMode,
                message = natMessage,
                submitEnabled = natSubmitEnabled,
                submitting = natSubmitting,
                onSelect = onSelectNatMode,
                onSave = onSaveNatMode,
            )

            // ── Declared region (fauna.admin.region.{get,set}) ──
            // The deployment's legal situs — the region tier's one human
            // choice (region-blocking.md § Region determination;
            // dynamic-features.md § The region tier). Every rendering
            // decision is the shared FfiAdminRegionView fold (tui's
            // admin/nest.rs, the reference leg) — this file paints exactly
            // what it hands back and wires two buttons; it decides nothing
            // about the plane. DECLARED, NEVER DETECTED (ratified
            // 2026-08-11): no detect/prefill affordance here.
            RegionSection(
                view = regionView,
                working = regionWorking,
                onSave = onSaveRegion,
                onWithdraw = onWithdrawRegion,
                onInvalidRegion = onInvalidRegion,
                parseRegionCode = parseRegionCode,
            )

            // ── Host-OS maintenance (nest-os-*, installers/vps.md § Host OS Maintenance § 4) ──
            // Read-only patch/reboot status for the host Ubuntu box of an
            // onboarded VPS, off the os_* fields on setup.status. The status line
            // is always present (shared os_maintenance_status_label, single-sourced
            // across all 7 apps); the count badge shows only when updates pend;
            // the restart-now button shows only when a reboot pends and drives
            // fauna.admin.request_host_restart. A nest with no host channel reads
            // the defaults → "OS up to date" (no false alarm).
            OsMaintenanceRow(
                securityUpdates = osSecurityUpdates,
                rebootPending = osRebootPending,
                restartingNow = restartingNow,
                onRestartNow = onRestartNow,
            )

            // ── Deployment-identity rotation (admin-nest-seed-rotate-*) ──
            // Give this nest a brand-new deployment identity — evicts every
            // previously-trusted holder's authority; the roster of admins who
            // inherit the new identity IS the confirm surface. Inline
            // expanding section (not a modal, unlike Factory reset below):
            // the roster is dynamic, indexed content that must keep
            // re-rendering while armed. Mirrors linux `views/admin.rs`'s
            // three-state shape exactly (`AdminNestVM.SeedRotateConfirmState`).
            SeedRotateSection(
                confirm = seedRotateConfirm,
                status = seedRotateStatus,
                onArm = onArmSeedRotate,
                onCancel = onCancelSeedRotate,
                onConfirm = onConfirmSeedRotate,
            )

            // ── Outside-app sign-in keys (admin-nest-oauth-*) ──
            // The nest-held OAuth issuer key set and its refresh-token secret
            // (authorization-server.md § The issuer → Two rotation arms):
            // directly after the deployment identity, the same class of
            // deployment crypto (admin.md § N Nest). No nav entry. Every
            // sentence is a shared fauna_client_admin fold; this paints and
            // wires. Mirrors tui's `admin/nest.rs::oauth_elements`.
            OauthKeysSection(
                keys = oauthKeys,
                armed = oauthArmed,
                status = oauthStatus,
                inFlight = oauthInFlight,
                issuerKeyRowLabel = issuerKeyRowLabel,
                issuerKeyRotateCost = issuerKeyRotateCost,
                onRotate = onRotateIssuerKey,
                onArm = onArmOauthForced,
                onCancel = onCancelOauthForced,
                onConfirm = onConfirmOauthForced,
            )

            HorizontalDivider()

            // ── Legal takedown (admin-nest-takedown-*) ──
            // The legal-compulsion console (moderation.md § Legal takedown →
            // Invocation surface, ruled 2026-08-16). The
            // draft fields are local (no other part of this page observes
            // them); the arm control's fold is pure, so it recomputes on
            // every recomposition with no async intermediate — simpler than
            // deployment-identity rotation above. Mirrors linux
            // `views/admin.rs`'s `build_nest_page` (the reference leg).
            TakedownSection(
                armed = takedownArmed,
                status = takedownStatus,
                takedownFormView = takedownFormView,
                onArm = onArmTakedown,
                onCancel = onCancelTakedown,
                onConfirm = onConfirmTakedown,
                prefill = takedownPrefill,
                onPrefillConsumed = onConsumeTakedownPrefill,
            )

            HorizontalDivider()

            // ── Reports queue (admin-nest-reports-*; moderation.md § User-initiated
            // reporting → *Where it lands*) — directly after the takedown console it
            // pre-fills, as ui.yaml's admin-nest element order declares.
            ReportsQueueSection(
                reports = reports,
                onResolve = onResolveReport,
                onOpenTakedown = onOpenReportTakedown,
            )

            HorizontalDivider()

            // ── Danger zone — Factory reset ──
            FactoryResetSection(onFactoryReset)

            // Per-page error surface (rule #2): a failed services.{list,update}
            // on the pairing path or a failed factory_reset routes here.
            //
            // No `else` placeholder — ui.yaml conformance is about the id EXISTING
            // on the page, not about it being permanently on screen. A shim
            // carrying this id must leave the tree when it has nothing to say
            // (`e2e-conventions.md` convention 2's rider, obligation (a)); the
            // bridge resolves visibility as bare existence in the semantics tree
            // (`ElementOps.isVisible` = `findAll(id).isNotEmpty()`), so the empty
            // `Box` this used to render made `is_visible("error-message")`
            // structurally true on a clean page.
            if (!error.isNullOrEmpty()) {
                Text(
                    error,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
                )
            }
        }
    }
}

/** The admin pairing row: title + description on the left, a status badge +
 *  reflective toggle on the right. The status text is the canonical
 *  Enabled/Disabled label (reused from the services_page namespace). */
@Composable
private fun PairingRow(
    checked: Boolean,
    working: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    // A dispatch-on-change toggle IS the commit, so it declares (the offline
    // gate's "the commit gates, not the buffer" rule — there is no Save beside
    // it to carry the declaration instead).
    val gate = faunaGate("fauna.admin.services.update", enabled = !working)
    Card(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    stringResource(R.string.admin_services_page_pairing),
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    stringResource(R.string.admin_services_page_pairing_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                stringResource(
                    if (checked) R.string.admin_services_page_enabled
                    else R.string.admin_services_page_disabled
                ),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.ADMIN_SERVICE_PAIRING_STATUS),
            )
            Switch(
                checked = checked,
                onCheckedChange = onCheckedChange,
                enabled = gate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_SERVICE_PAIRING_TOGGLE),
            )
        }
        DisabledControlReasonText(
            gate.reason,
            modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 12.dp),
        )
    }
}

/** The admin-set client-facing API serving-port field: a numeric text_input +
 *  save button (mirrors AdminCalendarScreen's `CaldavPortField`). The save
 *  validates the port via the shared `parsePort` (u16 in [1, 65535], reject 0)
 *  client-side — invalid → `onInvalidPort` (the VM
 *  surfaces the message on `error-message`, no dispatch), else `onSavePort`
 *  drives `fauna.admin.set_serving_port`. The entry seeds from the persisted
 *  port (`remember(servingPort)`), so a re-read after save re-seeds it. */
@Composable
private fun ServingPortField(
    servingPort: Int,
    frontedByRouter: Boolean,
    working: Boolean,
    onSavePort: (Int) -> Unit,
    onInvalidPort: () -> Unit,
    parsePort: (String) -> Int?,
) {
    var edited by remember(servingPort) { mutableStateOf(servingPort.toString()) }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = edited,
                onValueChange = { edited = it.filter(Char::isDigit) },
                label = { Text(stringResource(R.string.admin_nest_page_serving_port_label)) },
                supportingText = { Text(stringResource(R.string.admin_nest_page_serving_port_desc)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                // Read-only behind the cloud :443 SNI router — the chosen port is
                // inert and the nest rejects a write. nest/common.md § Serving ports.
                enabled = !working && !frontedByRouter,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_SERVING_PORT_INPUT),
            )
            if (frontedByRouter) {
                Text(
                    stringResource(R.string.admin_nest_page_serving_port_fronted_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // The commit gates; the input above it stays live so the admin can
            // still type. The page's own predicate is handed over rather than
            // re-tested beside the verdict.
            val gate = faunaGate(
                "fauna.admin.set_serving_port",
                enabled = !working && !frontedByRouter,
            )
            Button(
                onClick = {
                    val port = parsePort(edited)
                    if (port == null) {
                        onInvalidPort()
                    } else {
                        onSavePort(port)
                    }
                },
                enabled = gate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_NEST_SERVING_PORT_SAVE_BUTTON),
            ) { Text(stringResource(R.string.admin_nest_page_serving_port_save)) }
            DisabledControlReasonText(gate.reason)
        }
    }
}

/** The admin-nest NAT-mode control (admin.md § Nest → NAT-mode control): two
 *  radios + a status line + save button, driven by the shared
 *  `AdminNatModeMachine` snapshot. Radio rows use the selectable-row idiom
 *  (mirrors `PrivacySettingsScreen`'s inbox-mode radios) so the whole label —
 *  not just the small radio circle — is the click target. No defer button —
 *  navigating away is the defer (a post-onboarding flip does NOT re-derive
 *  the § 3b claim-time serving enables). */
@Composable
private fun NatModeSection(
    selectedMode: NodeMode,
    message: LocalizedText?,
    submitEnabled: Boolean,
    submitting: Boolean,
    onSelect: (NodeMode) -> Unit,
    onSave: () -> Unit,
) {
    // Save commits; the two radios above it are the buffer and stay live, so an
    // admin can still choose a mode while offline and commit on reconnect.
    val gate = faunaGate("fauna.setup.nat_mode", enabled = submitEnabled)
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_nat_mode_label),
                style = MaterialTheme.typography.titleMedium,
            )
            NatModeRadioRow(
                testTag = Ids.ADMIN_NEST_NAT_MODE_PUBLIC_RADIO,
                selected = selectedMode == NodeMode.PUBLIC,
                enabled = !submitting,
                onClick = { onSelect(NodeMode.PUBLIC) },
                label = stringResource(R.string.onboarding_nat_mode_public_label),
                desc = stringResource(R.string.onboarding_nat_mode_public_desc),
            )
            NatModeRadioRow(
                testTag = Ids.ADMIN_NEST_NAT_MODE_PRIVATE_RADIO,
                selected = selectedMode == NodeMode.PRIVATE,
                enabled = !submitting,
                onClick = { onSelect(NodeMode.PRIVATE) },
                label = stringResource(R.string.onboarding_nat_mode_private_label),
                desc = stringResource(R.string.onboarding_nat_mode_private_desc),
            )
            Row(
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    localized(message).orEmpty(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.weight(1f).testTag(Ids.ADMIN_NEST_NAT_MODE_STATUS),
                )
                Button(
                    onClick = onSave,
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_NAT_MODE_SAVE_BUTTON),
                ) { Text(stringResource(R.string.admin_nest_page_nat_mode_save)) }
            }
            DisabledControlReasonText(gate.reason)
        }
    }
}

@Composable
private fun NatModeRadioRow(
    testTag: String,
    selected: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
    label: String,
    desc: String,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .selectable(
                selected = selected,
                enabled = enabled,
                role = Role.RadioButton,
                onClick = onClick,
            )
            .testTag(testTag)
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        RadioButton(selected = selected, onClick = null, enabled = enabled)
        Column(Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyLarge)
            Text(desc, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/** The declared-region section (`admin-nest-region-*`,
 *  `fauna.admin.region.{get,set}`) — the deployment's legal situs, the region
 *  tier's one human choice (region-blocking.md § Region determination;
 *  dynamic-features.md § The region tier). Every rendering decision is the
 *  shared `FfiAdminRegionView` fold (tui `admin/nest.rs`, the reference leg)
 *  — this file paints exactly what it hands back and wires two buttons; it
 *  decides nothing about the plane. Mirrors linux `views/admin.rs`'s
 *  `build_nest_page` region block. [view] is `null` before the first read
 *  resolves — the fresh-install defaults (`REGION_NONE`, no authority/
 *  staleness/withdraw) render meanwhile, matching linux's synchronous
 *  pre-read seed.
 *
 *  ⚠ DECLARED, NEVER DETECTED (ratified 2026-08-11): no detect/prefill
 *  affordance may be added here — [parseRegionCode] deliberately does not
 *  even case-fold (two spellings of one region would both be storable). */
@Composable
private fun RegionSection(
    view: FfiAdminRegionView?,
    working: Boolean,
    onSave: (String) -> Unit,
    onWithdraw: () -> Unit,
    onInvalidRegion: () -> Unit,
    // Injected by the caller so the Robolectric content-test can pass an
    // FFI-free stub — mirrors AdminNestContent's `parsePort` param.
    parseRegionCode: (String) -> String?,
) {
    var edited by remember(view?.declared) { mutableStateOf(view?.declared.orEmpty()) }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_REGION_SECTION)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_region_label),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.admin_nest_page_region_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // `admin-nest-region-status` — the declared region, or that none
            // is declared. A NORMAL state, never an error: a deployment that
            // has never declared is conforming.
            Text(
                view?.let { localized(it.status) }
                    ?: stringResource(R.string.admin_nest_page_region_none),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.ADMIN_NEST_REGION_STATUS),
            )
            // `admin-nest-region-authority` — present only while a region is
            // declared: before that there is no authority channel to
            // describe, and inventing a line about one would be a claim.
            val authority = view?.authority?.let { localized(it) }
            if (authority != null) {
                Text(
                    authority,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_REGION_AUTHORITY),
                )
            }
            // `admin-nest-region-staleness` — the nest-reported "act when you
            // can" warning, only when the channel is unreached. The rules
            // already received stay in force, so this is a caveat, never an
            // outage.
            val staleness = view?.staleness?.let { localized(it) }
            if (staleness != null) {
                Text(
                    staleness,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_REGION_STALENESS),
                )
            }
            OutlinedTextField(
                value = edited,
                onValueChange = { edited = it },
                label = { Text(stringResource(R.string.admin_nest_page_region_placeholder)) },
                singleLine = true,
                enabled = !working,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_REGION_INPUT),
            )
            // The commit gates; the input above it stays live so the admin
            // can still type. Both save and withdraw drive the same wire
            // kind (`fauna.admin.region.set` with the region present/absent).
            val gate = faunaGate("fauna.admin.region.set", enabled = !working)
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Button(
                    onClick = {
                        val code = parseRegionCode(edited)
                        if (code == null) {
                            onInvalidRegion()
                        } else {
                            onSave(code)
                        }
                    },
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_REGION_SAVE_BUTTON),
                ) { Text(stringResource(R.string.admin_nest_page_region_save)) }
                // `admin-nest-region-withdraw-button` — shown only while a
                // region is declared. Withdrawing also retires the previous
                // region's feature-policy document nest-side.
                if (view?.canWithdraw == true) {
                    OutlinedButton(
                        onClick = onWithdraw,
                        enabled = gate.enabled,
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_REGION_WITHDRAW_BUTTON),
                    ) { Text(stringResource(R.string.admin_nest_page_region_withdraw)) }
                }
            }
            DisabledControlReasonText(gate.reason)
        }
    }
}

/** Host-OS maintenance row: the always-present localized status line
 *  (`nest-os-maintenance-status`, the shared `os_maintenance_status_label` decision
 *  resolved through android i18n — NOT hand-rolled, following the shared
 *  label-lift precedent), the raw pending-updates count badge (`nest-os-updates-count`, only
 *  when `securityUpdates > 0`, a focused integer split off the categorical line),
 *  and the "restart now" button (`nest-os-restart-now-button`, only when a reboot
 *  pends → `fauna.admin.request_host_restart`). Mirrors the web `+page.svelte`
 *  os-maintenance section + the linux `build_nest_page` os row. */
@Composable
private fun OsMaintenanceRow(
    securityUpdates: Int,
    rebootPending: Boolean,
    restartingNow: Boolean,
    onRestartNow: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    localized(
                        com.fauna.ffi.osMaintenanceStatusLabel(securityUpdates.toUInt(), rebootPending)
                    ).orEmpty(),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.NEST_OS_MAINTENANCE_STATUS),
                )
                if (securityUpdates > 0) {
                    Text(
                        securityUpdates.toString(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.NEST_OS_UPDATES_COUNT),
                    )
                }
            }
            if (rebootPending) {
                val gate = faunaGate(
                    "fauna.admin.request_host_restart",
                    enabled = !restartingNow,
                )
                Column {
                    Button(
                        onClick = onRestartNow,
                        enabled = gate.enabled,
                        modifier = Modifier.testTag(Ids.NEST_OS_RESTART_NOW_BUTTON),
                    ) { Text(stringResource(R.string.admin_nest_page_os_restart_now)) }
                    DisabledControlReasonText(gate.reason)
                }
            }
        }
    }
}

// ── Deployment-identity rotation (admin-nest-seed-rotate-*) ───────────
// Inline expanding section, not a modal (unlike Factory reset below): the
// roster is dynamic, indexed content that must keep re-rendering while armed
// — a transient AlertDialog can't hold that. Mirrors linux `views/admin.rs`'s
// three-state shape (`AdminNestVM.SeedRotateConfirmState`: Loading / Failed /
// Ready) exactly.

@Composable
private fun SeedRotateSection(
    confirm: AdminNestVM.SeedRotateConfirmState?,
    status: String?,
    onArm: () -> Unit,
    onCancel: () -> Unit,
    onConfirm: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_SEED_ROTATE_SECTION)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_rotate_seed_label),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.admin_nest_page_rotate_seed_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (confirm == null) {
                Button(
                    onClick = onArm,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_BUTTON),
                ) { Text(stringResource(R.string.admin_nest_page_rotate_seed_button)) }
            } else {
                Text(
                    stringResource(R.string.admin_nest_page_rotate_seed_confirm_body),
                    style = MaterialTheme.typography.bodySmall,
                )
                // Roster-item / roster-reason are mutually exclusive
                // (ui.yaml's ordering rule): Loading/Failed show a reason,
                // never roster rows; Ready shows roster rows, and a reason
                // only when the nest withholds confirm.
                when (confirm) {
                    is AdminNestVM.SeedRotateConfirmState.Loading -> {
                        Text(
                            stringResource(R.string.admin_nest_page_rotate_seed_roster_loading),
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_ROSTER_REASON),
                        )
                    }
                    is AdminNestVM.SeedRotateConfirmState.Failed -> {
                        Text(
                            confirm.message,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_ROSTER_REASON),
                        )
                    }
                    is AdminNestVM.SeedRotateConfirmState.Ready -> {
                        confirm.view.inheritors.forEachIndexed { i, inheritor ->
                            Text(
                                inheritor.label,
                                style = MaterialTheme.typography.bodyMedium,
                                modifier = Modifier.testTag("${Ids.ADMIN_NEST_SEED_ROTATE_ROSTER_ITEM}-$i"),
                            )
                        }
                        val reason = localized(confirm.view.blockedReason)
                        if (reason != null) {
                            Text(
                                reason,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                                modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_ROSTER_REASON),
                            )
                        }
                    }
                }
                // The confirm commits; the ARM button above declares nothing,
                // deliberately — arming resolves the inheritor roster through
                // `fauna.admin.admins.list`, a `Read`, which ruling 1 never
                // greys. Declaring it would gate nothing while reading as proof
                // the control is gated (web reached the same conclusion on this
                // exact section). The cancel beside it is pure local UI.
                val gate = faunaGate(
                    "fauna.admin.deployment_seed.rotate",
                    enabled = confirm is AdminNestVM.SeedRotateConfirmState.Ready &&
                        confirm.view.canConfirm,
                )
                Row(
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    OutlinedButton(
                        onClick = onCancel,
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_CANCEL_BUTTON),
                    ) { Text(stringResource(R.string.admin_nest_page_rotate_seed_cancel_button)) }
                    OutlinedButton(
                        onClick = onConfirm,
                        enabled = gate.enabled,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_CONFIRM_BUTTON),
                    ) { Text(stringResource(R.string.admin_nest_page_rotate_seed_confirm_button)) }
                }
                DisabledControlReasonText(gate.reason)
            }
            if (status != null) {
                Text(
                    status,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_SEED_ROTATE_STATUS),
                )
            }
        }
    }
}

// ── Outside-app sign-in keys (authorization-server.md § The issuer) ────────────
// The compromise response over the nest-held OAuth issuer key set and its
// second signer, the refresh-token secret: the served keys, the ordinary
// rotation (no confirm — nothing breaks, so its cost is stated beside it), and
// the two forced arms behind ONE shared inline confirm that states the armed
// arm's cost before dispatch. Paint only — every sentence comes from a shared
// fold ([issuerKeyRowLabel], [issuerKeyRotateCost], the confirm captured at arm
// time, the verdicts), and `AdminNestVM` refuses each gesture on the same test
// the `enabled` flags below read. Mirrors tui's `admin/nest.rs::oauth_elements`
// (the lead app) element for element.

@Composable
private fun OauthKeysSection(
    keys: AdminNestVM.OauthKeysRead,
    armed: AdminNestVM.OauthArmed?,
    status: String?,
    inFlight: Boolean,
    issuerKeyRowLabel: (FfiIssuerKeyRow, Long) -> LocalizedText,
    issuerKeyRotateCost: (FfiIssuerKeyView) -> LocalizedText,
    onRotate: () -> Unit,
    onArm: (FfiIssuerForcedArm) -> Unit,
    onCancel: () -> Unit,
    onConfirm: (FfiIssuerForcedArm) -> Unit,
) {
    val view = (keys as? AdminNestVM.OauthKeysRead.Ready)?.view
    // All three controls are live exactly when the set has answered and no
    // call is in flight — disabled, never hidden, beside the reason line
    // otherwise (the forced confirm could not name what it drops).
    val live = view != null && !inFlight

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_OAUTH_SECTION)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_oauth_label),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.admin_nest_page_oauth_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // The key rows, painted ONLY from an answered read (the seed-rotate
            // roster's rule): "not asked yet" and "couldn't find out" get the
            // reason line instead, never an empty list that would read as "no
            // keys". Signer first, in the nest's own order — never re-sorted.
            when (keys) {
                is AdminNestVM.OauthKeysRead.Ready -> {
                    // The countdown is the point of a retired key's line, so it
                    // is counted against the wall clock at paint; the instant it
                    // counts to is the nest's own.
                    val nowSecs = System.currentTimeMillis() / 1000
                    keys.view.keys.forEachIndexed { i, row ->
                        Text(
                            localized(issuerKeyRowLabel(row, nowSecs)).orEmpty(),
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.testTag("${Ids.ADMIN_NEST_OAUTH_KEY_ITEM}-$i"),
                        )
                    }
                }
                is AdminNestVM.OauthKeysRead.Failed -> {
                    Text(
                        keys.reason,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_KEY_REASON),
                    )
                }
                AdminNestVM.OauthKeysRead.Unread -> {
                    Text(
                        stringResource(R.string.admin_nest_page_oauth_keys_loading),
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_KEY_REASON),
                    )
                }
            }

            // The ordinary arm states its cost beside itself: it has no confirm.
            if (view != null) {
                Text(
                    localized(issuerKeyRotateCost(view)).orEmpty(),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            // The ordinary arm commits on the press, so it declares. The two
            // arm buttons below dispatch nothing (arming is local) and declare
            // nothing — the confirm they open is what commits, and gates.
            val rotateGate = faunaGate("fauna.oauth.rotate_issuer_key", enabled = live)
            Button(
                onClick = onRotate,
                enabled = rotateGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_ROTATE_BUTTON),
            ) { Text(stringResource(R.string.admin_nest_page_oauth_rotate_button)) }
            DisabledControlReasonText(rotateGate.reason)
            OutlinedButton(
                onClick = { onArm(FfiIssuerForcedArm.ISSUER_KEY) },
                enabled = live,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_FORCE_ROTATE_BUTTON),
            ) { Text(stringResource(R.string.admin_nest_page_oauth_force_rotate_button)) }
            OutlinedButton(
                onClick = { onArm(FfiIssuerForcedArm.SESSION_SECRET) },
                enabled = live,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_SECRET_FORCE_ROTATE_BUTTON),
            ) { Text(stringResource(R.string.admin_nest_page_oauth_secret_force_rotate_button)) }

            // The armed confirm renders the CAPTURED fold, and its confirm
            // carries the arm it was rendered for (the VM refuses a mismatch).
            if (armed != null) {
                Text(
                    localized(armed.confirm.summary).orEmpty(),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_CONFIRM_SUMMARY),
                )
                // The confirm issues exactly the armed arm's kind, so it
                // declares that kind — both literals spelled out for the
                // offline-gate-kinds check, the discriminant choosing between
                // them (OfflineGate.kt's `faunaGate(if (…) "a" else "b")` shape).
                val confirmGate = faunaGate(
                    when (armed.arm) {
                        FfiIssuerForcedArm.ISSUER_KEY -> "fauna.oauth.force_rotate_issuer_key"
                        FfiIssuerForcedArm.SESSION_SECRET -> "fauna.oauth.force_rotate_session_secret"
                    }
                )
                Row(
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    OutlinedButton(
                        onClick = onCancel,
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_CANCEL_BUTTON),
                    ) { Text(stringResource(R.string.admin_nest_page_oauth_cancel_button)) }
                    OutlinedButton(
                        onClick = { onConfirm(armed.arm) },
                        enabled = confirmGate.enabled,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_CONFIRM_BUTTON),
                    ) { Text(localized(armed.confirm.confirmLabel).orEmpty()) }
                }
                DisabledControlReasonText(confirmGate.reason)
            }

            // The verdict, its own element — never the page's `error-message`:
            // every success here has consequences worth words, and a failure
            // must not claim nothing changed.
            if (status != null) {
                Text(
                    status,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_OAUTH_STATUS),
                )
            }
        }
    }
}

// ── Legal takedown (moderation.md § Legal takedown → Invocation surface) ───────
// The legal-compulsion console. Every gating/wording decision is the shared
// `fauna_client_moderation::takedown` fold ([takedownFormView]/
// `takedownVerdict`, pure) — this composable paints exactly what the fold
// hands back and wires the arm→confirm→dispatch shape; it decides nothing.
// Simpler than deployment-identity rotation above: no async intermediate, so
// the arm control's sensitivity + reason recompute on every recomposition.

@Composable
private fun TakedownSection(
    armed: AdminNestVM.TakedownArmed?,
    status: String?,
    takedownFormView: (String, Boolean, String, Boolean) -> com.fauna.ffi.FfiTakedownFormView,
    onArm: (String, Boolean, String, Boolean, com.fauna.ffi.FfiTakedownFormView) -> Unit,
    onCancel: () -> Unit,
    onConfirm: () -> Unit,
    prefill: com.fauna.ffi.FfiTakedownPrefill? = null,
    onPrefillConsumed: () -> Unit = {},
) {
    var contentId by remember { mutableStateOf("") }
    var conversation by remember { mutableStateOf(false) }
    var reference by remember { mutableStateOf("") }
    var restore by remember { mutableStateOf(false) }
    val view = remember(contentId, conversation, reference, restore) {
        takedownFormView(contentId, conversation, reference, restore)
    }

    // A report row's *open takedown* pre-fills the console (the draft fields are
    // local state, so the request arrives as a one-shot): the content id and kind
    // only — NO citation, so the console's own legal-reference guard still stands.
    LaunchedEffect(prefill) {
        prefill?.let {
            contentId = it.contentId
            conversation = it.conversation
            onPrefillConsumed()
        }
    }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_TAKEDOWN_SECTION)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_takedown_label),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.admin_nest_page_takedown_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedTextField(
                value = contentId,
                onValueChange = { contentId = it },
                label = { Text(stringResource(R.string.admin_nest_page_takedown_content_id_label)) },
                singleLine = true,
                enabled = armed == null,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_TAKEDOWN_CONTENT_ID_INPUT),
            )
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .selectable(
                        selected = !conversation,
                        enabled = armed == null,
                        role = Role.RadioButton,
                        onClick = { conversation = false },
                    )
                    .testTag(Ids.ADMIN_NEST_TAKEDOWN_TYPE_POST_RADIO),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = !conversation, onClick = null, enabled = armed == null)
                Text(stringResource(R.string.admin_nest_page_takedown_type_post))
            }
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .selectable(
                        selected = conversation,
                        enabled = armed == null,
                        role = Role.RadioButton,
                        onClick = { conversation = true },
                    )
                    .testTag(Ids.ADMIN_NEST_TAKEDOWN_TYPE_CONVERSATION_RADIO),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = conversation, onClick = null, enabled = armed == null)
                Text(stringResource(R.string.admin_nest_page_takedown_type_conversation))
            }
            OutlinedTextField(
                value = reference,
                onValueChange = { reference = it },
                label = { Text(stringResource(R.string.admin_nest_page_takedown_reference_label)) },
                singleLine = true,
                enabled = armed == null,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_TAKEDOWN_REFERENCE_INPUT),
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = restore,
                    onCheckedChange = { restore = it },
                    enabled = armed == null,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_TAKEDOWN_RESTORE_CHECKBOX),
                )
                Text(stringResource(R.string.admin_nest_page_takedown_restore_label))
            }
            if (armed == null) {
                // The button renders disabled with the fold's stated reason
                // (walk I5's spirit) — never a dead control with no reason.
                val gate = faunaGate("fauna.moderation.legal_takedown", enabled = view.canSubmit)
                Button(
                    onClick = { onArm(contentId, conversation, reference, restore, view) },
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_TAKEDOWN_BUTTON),
                ) { Text(localized(view.armLabel) ?: "") }
                DisabledControlReasonText(gate.reason ?: localized(view.blockedReason))
            } else {
                Text(
                    localized(armed.view.confirmSummary) ?: "",
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_TAKEDOWN_CONFIRM_SUMMARY),
                )
                Row(
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    val gate = faunaGate("fauna.moderation.legal_takedown")
                    OutlinedButton(
                        onClick = onCancel,
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_TAKEDOWN_CANCEL_BUTTON),
                    ) { Text(stringResource(R.string.admin_nest_page_takedown_cancel_button)) }
                    OutlinedButton(
                        onClick = onConfirm,
                        enabled = gate.enabled,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_NEST_TAKEDOWN_CONFIRM_BUTTON),
                    ) { Text(localized(armed.view.confirmLabel) ?: "") }
                }
            }
            if (status != null) {
                Text(
                    status,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ADMIN_NEST_TAKEDOWN_STATUS),
                )
            }
        }
    }
}

// ── Reports queue (moderation.md § User-initiated reporting → Where it lands) ──
// The open abuse reports, local and forwarded, oldest first — already worded by
// the shared `queue_row_view`. One flat `admin-nest-report-item` per row with its
// three levers: *open takedown* (posts and messages only — the shared
// `can_open_takedown`) pre-fills the console above with NO citation; *acted* /
// *dismiss* only RECORD the outcome (the reporter is told nothing more), over
// `fauna.moderation.abuse_report.resolve` — OnlineOnly, so each declares it.
// The empty line paints only off the `loaded` bit (loading is not empty,
// `ui/README.md` § List pages).

@Composable
private fun ReportsQueueSection(
    reports: AdminNestVM.ReportsState,
    onResolve: (com.fauna.ffi.FfiReportQueueRow, Boolean) -> Unit,
    onOpenTakedown: (com.fauna.ffi.FfiReportQueueRow) -> Unit,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_NEST_REPORTS_SECTION)) {
        Column(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.admin_nest_page_reports_label),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.admin_nest_page_reports_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (!reports.loaded) {
                Text(
                    stringResource(R.string.admin_nest_page_reports_loading),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (reports.rows.isEmpty()) {
                Text(
                    stringResource(R.string.admin_nest_page_reports_empty),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            reports.rows.forEach { row ->
                key(row.reportId) {
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(
                            AdminNestVM.reportLine(row) { resolveLocalized(context, it).orEmpty() },
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.testTag(Ids.ADMIN_NEST_REPORT_ITEM),
                        )
                        val gate = faunaGate("fauna.moderation.abuse_report.resolve")
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (row.canOpenTakedown) {
                                OutlinedButton(
                                    onClick = { onOpenTakedown(row) },
                                    modifier = Modifier.testTag(Ids.ADMIN_NEST_REPORT_OPEN_TAKEDOWN_BUTTON),
                                ) { Text(stringResource(R.string.admin_nest_page_reports_open_takedown)) }
                            }
                            OutlinedButton(
                                onClick = { onResolve(row, true) },
                                enabled = gate.enabled,
                                modifier = Modifier.testTag(Ids.ADMIN_NEST_REPORT_ACTED_BUTTON),
                            ) { Text(stringResource(R.string.admin_nest_page_reports_acted)) }
                            OutlinedButton(
                                onClick = { onResolve(row, false) },
                                enabled = gate.enabled,
                                modifier = Modifier.testTag(Ids.ADMIN_NEST_REPORT_DISMISS_BUTTON),
                            ) { Text(stringResource(R.string.admin_nest_page_reports_dismiss)) }
                        }
                        DisabledControlReasonText(gate.reason)
                    }
                }
            }
            reports.status?.let {
                Text(it, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

// ── Danger zone — Factory reset ────────────────────────────────────────────────
// Moved off Settings (admin.md § Admin IA redesign): a nest-wide concern. The
// confirm button lives in a transient AlertDialog (ui.yaml optional_elements),
// present only while the dialog is open.

@Composable
private fun FactoryResetSection(onFactoryReset: () -> Unit) {
    var showDialog by remember { mutableStateOf(false) }

    // ARMING IS LOCAL, the confirm commits — so the opener below stays live with
    // no nest and only the dialog's confirm declares. Declared out here rather
    // than inside the `confirmButton` slot so the dialog BODY can carry the
    // reason; a Compose `AlertDialog` slot would see the same verdict either way
    // (its sub-composition inherits CompositionLocals from where the dialog is
    // declared — the reason apple's rule 3, which polices exactly this position
    // for SwiftUI, has nothing to police on Compose), and that inheritance is
    // measured rather than assumed by
    // `OfflineGateTest.aGateDeclaredInsideAnAlertDialogSlotStillSeesTheState`.
    val gate = faunaGate("fauna.admin.factory_reset")

    Column(modifier = Modifier.testTag(Ids.ADMIN_FACTORY_RESET_SECTION)) {
        Text(
            stringResource(R.string.admin_settings_page_factory_reset_section),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp),
        )
        Text(
            stringResource(R.string.admin_settings_page_factory_reset_title),
            style = MaterialTheme.typography.bodyMedium,
        )
        Text(
            stringResource(R.string.admin_settings_page_factory_reset_desc),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(vertical = 4.dp),
        )
        OutlinedButton(
            onClick = { showDialog = true },
            colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.testTag(Ids.ADMIN_FACTORY_RESET_BUTTON),
        ) {
            Text(stringResource(R.string.admin_settings_page_factory_reset_button))
        }
    }

    if (showDialog) {
        AlertDialog(
            onDismissRequest = { showDialog = false },
            title = { Text(stringResource(R.string.admin_settings_page_factory_reset_confirm_title)) },
            text = {
                Column {
                    Text(stringResource(R.string.admin_settings_page_factory_reset_confirm_body))
                    // The reason rides the dialog BODY rather than the button
                    // slot — a confirm slot has no room for a caption, and this
                    // is still beside the control the user is looking at
                    // (ui/README.md § Copy comprehensibility rule 5), never a
                    // global banner (§ R11 (account-data-plane.md § The ratified decisions)).
                    DisabledControlReasonText(gate.reason)
                }
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        showDialog = false
                        onFactoryReset()
                    },
                    enabled = gate.enabled,
                    colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ADMIN_FACTORY_RESET_CONFIRM_BUTTON),
                ) {
                    Text(stringResource(R.string.admin_settings_page_factory_reset_confirm_button))
                }
            },
            dismissButton = {
                TextButton(onClick = { showDialog = false }) {
                    Text(stringResource(R.string.admin_settings_page_factory_reset_cancel))
                }
            },
        )
    }
}
