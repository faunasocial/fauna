package com.fauna.app.ui.screen.backups

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.BackupDestinationsVM
import com.fauna.ffi.FfiBackupDestinationStatus
import com.fauna.ffi.FfiBackupDestinationView
import com.fauna.ffi.FfiDestinationAuditRow
import com.fauna.ffi.backupAuditAlertLabel
import com.fauna.ffi.backupBacklogLabel
import com.fauna.ffi.backupDestinationKindLabel
import com.fauna.ffi.backupDestinationKindOptions
import com.fauna.ffi.backupDestinationLabel
import com.fauna.ffi.backupLastAuditLabel
import com.fauna.ffi.backupLastUploadLabel
import com.fauna.ffi.backupSelfAuditIsAlerting
import com.fauna.ffi.backupSelfAuditLabel
import com.fauna.ffi.backupUsageLabel
import com.fauna.ffi.destinationKindClientDevice
import com.fauna.ffi.destinationRowIsAClientDevice
import com.fauna.ffi.everyDestinationIsAClientDevice
import com.fauna.ffi.orphanedStoreLabel
import com.fauna.ffi.parseByteSize
import social.fauna.generated.Ids
import uniffi.fauna_core.BackupAuditAlertReason

/**
 * The backup-destination management section on the Backups page
 * (`docs/goal/ui/backups.md` § Manage backup destinations, ratified 2026-06-14):
 * add (resolve identity → record), edit (rename / change URL), remove, plus the
 * per-destination status rows. Lifts the Linux lead
 * (apps/fauna-linux/src/views/backups/destinations.rs) onto Compose (priority #1
 * uniform; same ui.yaml IDs).
 *
 * The per-row LIVE status (last-upload / backlog) is read from the shared
 * `BackupCoordinator::destination_status()` over the gated FFI fn
 * `backup_destination_status` (via the VM) and rendered here — uniform with
 * linux/apple/windows (backups.md § Per-destination status read). Until a per-app
 * always-on upload coordinator runs, `last_upload_time` reads "never" while the
 * backlog is a live count. Both row texts resolve through the shared
 * `fauna_core::format::{backup_last_upload_label,backup_backlog_label}` (the
 * never-vs-real decision, the epoch-0 guard, and the seconds→ms conversion all live
 * once in shared Rust — value-formatting.md § Backup destination status labels), so
 * they are injected as [lastUploadText]/[backlogText] — keeping the FFI off the
 * Robolectric-testable [BackupDestinationsContent] (the same idiom as
 * [RestoreHistorySection]'s `whenLabel`).
 *
 * Stateless [BackupDestinationsContent] is split out for the Robolectric harness;
 * the VM-bound [BackupDestinationsSection] is what the Backups page mounts.
 *
 * **The audit-alert surface** (backups.md § Audit-alert surface) rides the
 * same [BackupDestinationsVM]: the per-row `backup-destination-last-audit-time`
 * is threaded through here like its upload sibling, but the indexed
 * `backup-audit-alert` banners are a separately exported composable,
 * [BackupAuditAlerts] — see its doc for why.
 */
@Composable
fun BackupDestinationsSection(
    vm: BackupDestinationsVM = hiltViewModel(),
) {
    val destinations by vm.destinations.collectAsState()
    val statuses by vm.statuses.collectAsState()
    val auditRows by vm.auditRows.collectAsState()
    val working by vm.working.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val orphanedStoreBytes by vm.orphanedStoreBytes.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    // The shared FFI returns the EDIT_DIFFERENT_NEST_ERR sentinel for a
    // different-nest URL edit; localize it before showing (i18n stays in the view).
    val differentNest = stringResource(R.string.backups_backup_destination_edit_different_nest)
    // The shared reclaim reports `still_hosting` as an OUTCOME, not an error —
    // nothing was deleted — so the VM carries it here as its own sentinel to be
    // localized beside the one above.
    val stillHosting = stringResource(R.string.backups_backup_reclaim_still_hosting)
    LaunchedEffect(errorMessage) {
        errorMessage?.let {
            val shown = when (it) {
                BackupDestinationsVM.EDIT_DIFFERENT_NEST_ERR -> differentNest
                BackupDestinationsVM.RECLAIM_STILL_HOSTING -> stillHosting
                else -> it
            }
            appMessages.showError(shown)
        }
    }

    LaunchedEffect(Unit) { vm.refresh() }

    BackupDestinationsContent(
        destinations = destinations,
        statuses = statuses,
        working = working,
        onAdd = vm::add,
        onEdit = vm::edit,
        onRemove = vm::remove,
        // `backup-destination-last-upload-time` text: the never-vs-real decision +
        // epoch-0 guard + seconds→ms conversion live once in shared Rust
        // (backupLastUploadLabel); the returned label's `{when}` placeholder is
        // filled by resolving the ALREADY-COMPUTED `when` display (no second FFI
        // round-trip) via ValueFormat.render. FFI lives here, off the testable Content.
        lastUploadText = { status ->
            val lastUploadSecs = status?.lastUploadTime
            val display = backupLastUploadLabel(lastUploadSecs, System.currentTimeMillis())
            var label = resolveLocalized(context, display.label) ?: ""
            display.`when`?.let { when_ ->
                val fallbackMs = (lastUploadSecs ?: 0u).toLong() * 1000
                label = label.replace("{when}", ValueFormat.render(context, when_, fallbackMs))
            }
            label
        },
        // `backup-destination-backlog-count` text via the shared backupBacklogLabel
        // (absent status ⇒ the shared 0 baseline). FFI lives here, off the testable Content.
        backlogText = { status -> resolveLocalized(context, backupBacklogLabel(status?.backlogCount)) ?: "" },
        // `backup-destination-last-audit-time` text: the audit twin of
        // `lastUploadText`, resolved exactly the same way, off this client's OWN
        // audit pass rather than the source nest's report (backups.md
        // § Audit-alert surface). `None` (no pass yet) ⇒ "never".
        lastAuditText = { row ->
            val lastPassedSecs = row?.lastPassedAt
            val display = backupLastAuditLabel(lastPassedSecs, System.currentTimeMillis())
            var label = resolveLocalized(context, display.label) ?: ""
            display.`when`?.let { when_ ->
                val fallbackMs = (lastPassedSecs ?: 0u).toLong() * 1000
                label = label.replace("{when}", ValueFormat.render(context, when_, fallbackMs))
            }
            label
        },
        // `backup-destination-last-audit-time` text for a **client-device
        // custodian** row: the device's own self-audit, via the separate
        // shared door `backupSelfAuditLabel` (backups.md § Audit-alert
        // surface → *The client-device arm*). Resolved exactly like
        // `lastAuditText`, off `status.lastAuditPassedAt` rather than an
        // audit row — a custodian has no owner-side pass to report.
        selfAuditText = { status ->
            val lastPassedSecs = status?.lastAuditPassedAt
            val display = backupSelfAuditLabel(lastPassedSecs, System.currentTimeMillis())
            var label = resolveLocalized(context, display.label) ?: ""
            display.`when`?.let { when_ ->
                val fallbackMs = (lastPassedSecs ?: 0u).toLong() * 1000
                label = label.replace("{when}", ValueFormat.render(context, when_, fallbackMs))
            }
            label
        },
        auditRows = auditRows,
        // Row label = display_name-else-host via shared `backup_destination_label`
        // (backups.md § Where logic lives); FFI lives here, off the testable Content.
        destinationLabel = { dest -> backupDestinationLabel(dest.displayName, dest.destinationNestUrl) },
        // ── The client-device destination kind (backups.md § Third destination kind) ──
        onEnrollCustodian = vm::enrollCustodian,
        // The shared kind catalog, in paint order (nest first), with each option's
        // client-device-ness resolved HERE against the exported discriminator —
        // the one place on this client that names it. `remember` because the
        // catalog is a constant of the build, not of the render.
        kindOptions = remember(context) {
            val clientDevice = destinationKindClientDevice()
            backupDestinationKindOptions().map { option ->
                DestinationKindOption(
                    value = option.value,
                    label = resolveLocalized(context, option.label) ?: option.value,
                    isClientDevice = option.value == clientDevice,
                )
            }
        },
        // `backup-destination-kind-badge`: the row's own discriminator through
        // the shared label, so an unknown kind renders as ITSELF rather than
        // masquerading as a nest.
        kindBadgeText = { dest -> resolveLocalized(context, backupDestinationKindLabel(dest.kind)) ?: "" },
        // `backup-destination-usage`: held vs cap, resolved with the same
        // two-level shape as lastUploadText — the `{held}`/`{cap}` placeholders
        // are filled from the ALREADY-COMPUTED byte-size displays.
        //
        // ⚠ `capState` rides through untouched. Cap-reached is read, never
        // inferred: a pass that stopped AT its cap ends below it, so
        // `held >= cap` would render a stalled backup as healthy-with-room.
        usageText = { dest, status ->
            val display = backupUsageLabel(status?.heldBytes, dest.capacityCapBytes, status?.capState)
            var label = resolveLocalized(context, display.label) ?: ""
            display.held?.let { label = label.replace("{held}", resolveLocalized(context, it) ?: "") }
            display.cap?.let { label = label.replace("{cap}", resolveLocalized(context, it) ?: "") }
            label
        },
        // The typed shared rule over the row this render already holds — never a
        // `kind == "client-device"` string match, which is how the Inert arm
        // (a client-device row with no device id) gets lost.
        //
        // The row-level door `row_is_a_client_device` rather than the
        // sole-client policy predicate over a one-element list: the two agree on
        // a singleton, but this is the one the reclaim contract names, and the
        // policy fn's empty-list arm has nothing to do with a single row.
        isClientDevice = { dest -> destinationRowIsAClientDevice(dest) },
        // The sole-client policy predicate over the whole list. Shared because
        // both its arms are policy answers: an empty list is NOT sole-client (an
        // account with no backup at all is not a durability warning), and an
        // unrecognised kind counts as NOT a client device (it may well BE the
        // off-site copy the warning would otherwise deny the user has).
        soleClientDestinations = everyDestinationIsAClientDevice(destinations),
        // The shared `parse_byte_size` — liberal in what a person may type,
        // strict about what counts as a number. `null` is a REFUSAL the user
        // sees, never a substituted default.
        parseCapacity = { typed -> parseByteSize(typed) },
        // The one `error-message` surface on this page — the same shell banner
        // every other failure in this section reaches (via the VM's errorMessage).
        onError = appMessages::showError,
        // ── Reclaim this device's copy ──
        // The VERDICT, already reached in the VM's op against this device's sync
        // id and its own disk read. Nothing is recomputed on the render path.
        orphanedStoreBytes = orphanedStoreBytes,
        // `backup-orphaned-store-row` sentence, two-level like `usageText`: the
        // `{held}` slot takes the ALREADY-resolved byte size, so the unit
        // localizes before it lands inside the sentence.
        orphanedStoreText = { held ->
            val display = orphanedStoreLabel(held)
            (resolveLocalized(context, display.label) ?: "")
                .replace("{held}", resolveLocalized(context, display.held) ?: "")
        },
        onReclaimStore = vm::reclaimStore,
    )
}

/**
 * The `backup-audit-alert` banners (`docs/goal/ui/backups.md` § Audit-alert
 * surface) — one per destination in a failing state, none at all when
 * everything is healthy (the `restore-divergence-banner` idiom). Which
 * verdicts are loud is **not** decided here: `FfiDestinationAuditRow.alertReasons`
 * is the single shared answer, so this client cannot drift into alerting on,
 * say, a transient `Unreachable` (the laptop-on-a-plane case the loop
 * deliberately keeps quiet).
 *
 * **Exported separately from [BackupDestinationsSection]** so the caller can
 * mount it OUTSIDE/ABOVE the snapshot-list scroller — a warning that a backup
 * is not keeping up must be visible without scrolling past the snapshot list
 * to find it (linux/web mount it the same way). Shares [BackupDestinationsVM]
 * with [BackupDestinationsSection] via Compose Navigation's per-destination
 * `ViewModelStoreOwner` scoping (both `hiltViewModel()` calls resolve to the
 * same instance within the `backups` nav destination) — one audit round trip,
 * one owner, mirroring linux's single `Ctx`.
 *
 * Stateless [BackupAuditAlertsContent] is split out for the Robolectric
 * harness, the same idiom [BackupDestinationsContent] uses.
 */
@Composable
fun BackupAuditAlerts(
    vm: BackupDestinationsVM = hiltViewModel(),
) {
    val destinations by vm.destinations.collectAsState()
    val statuses by vm.statuses.collectAsState()
    val auditRows by vm.auditRows.collectAsState()
    val context = LocalContext.current

    val alerts = destinations.flatMap { dest ->
        val label = backupDestinationLabel(dest.displayName, dest.destinationNestUrl)
        auditAlertReasons(auditRows[dest.destinationId]).map { reason ->
            resolveLocalized(context, backupAuditAlertLabel(reason, label)) ?: ""
        }
    } + destinations.filter { dest ->
        // A client-device custodian reporting its OWN copy as failing — the
        // only failure signal that exists for a kind the owner-side loop can
        // never sample (backup-destinations.md § Custodian contract,
        // question 4). Which reported states are loud stays single-sourced in
        // the shared `backupSelfAuditIsAlerting`.
        backupSelfAuditIsAlerting(statuses[dest.destinationId]?.auditState)
    }.map { dest ->
        val label = backupDestinationLabel(dest.displayName, dest.destinationNestUrl)
        resolveLocalized(context, backupAuditAlertLabel(BackupAuditAlertReason.SelfReported, label)) ?: ""
    }

    BackupAuditAlertsContent(alerts)
}

/**
 * The owner-side banner reasons for one destination's audit row — every entry
 * of `FfiDestinationAuditRow.alertReasons`, the shared door's single answer
 * (`DestinationAuditRecord::alert_reasons`): the standing verdict's reason, then
 * at most one `SourceRegressed` recovery-window notice. Each reason is opaque
 * here and goes straight into `backupAuditAlertLabel`.
 */
internal fun auditAlertReasons(row: FfiDestinationAuditRow?): List<BackupAuditAlertReason> =
    row?.alertReasons ?: emptyList()

/** Stateless render of the `backup-audit-alert` banners — see [BackupAuditAlerts]. */
@Composable
fun BackupAuditAlertsContent(alerts: List<String>) {
    alerts.forEach { alert ->
        Text(
            text = alert,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 4.dp)
                .testTag(Ids.BACKUP_AUDIT_ALERT),
        )
    }
}

/** Local add/edit-dialog mode for the destination form. */
private sealed interface FormMode {
    object Closed : FormMode
    object Add : FormMode
    data class Edit(val view: FfiBackupDestinationView) : FormMode
}

/**
 * One arm of `backup-destination-kind-select` — the wire discriminator a chosen
 * option writes to the row, plus the text to paint for it.
 *
 * [label] is deliberately **the same text** the resulting row's
 * `backup-destination-kind-badge` will carry: both come from the shared catalog
 * (`fauna_core::format::backup_destination_kind_options`, whose labels ARE the
 * badge labels), so the option a user picks and the badge they get back cannot
 * drift. Never pair a hand-written option list against the badge label here.
 *
 * [isClientDevice] is resolved **once**, in [BackupDestinationsSection], by
 * comparing [value] against the exported `destinationKindClientDevice()` — so
 * the shared discriminator is named in exactly one place on this client and the
 * form reads a boolean rather than string-matching `"client-device"` itself.
 * (That private mirror is the drift the `default_lapse_tier()` export exists to
 * delete elsewhere in this app.) Carrying it on the option, rather than passing
 * the raw discriminator down, also keeps the stateless content FFI-free for the
 * Robolectric harness.
 */
data class DestinationKindOption(
    val value: String,
    val label: String,
    val isClientDevice: Boolean,
)

@Composable
fun BackupDestinationsContent(
    destinations: List<FfiBackupDestinationView>,
    working: Boolean,
    onAdd: (String, String) -> Unit,
    onEdit: (String, String, String) -> Unit,
    /**
     * Remove a destination. The second argument is the
     * `backup-destination-remove-reclaim-checkbox` opt-in — "also free this
     * device's copy now", offered on client-device rows only. The reclaim runs
     * strictly after a successful deregister; that ordering is the callee's, not
     * this composable's.
     */
    onRemove: (String, Boolean) -> Unit,
    statuses: Map<String, FfiBackupDestinationStatus> = emptyMap(),
    lastUploadText: (FfiBackupDestinationStatus?) -> String = { "" },
    backlogText: (FfiBackupDestinationStatus?) -> String = { "" },
    auditRows: Map<String, FfiDestinationAuditRow> = emptyMap(),
    lastAuditText: (FfiDestinationAuditRow?) -> String = { "" },
    /**
     * `backup-destination-last-audit-time` text for a **client-device
     * custodian** row: its own self-audit, off the status row rather than an
     * audit row (backups.md § Audit-alert surface → *The client-device arm*).
     */
    selfAuditText: (FfiBackupDestinationStatus?) -> String = { "" },
    destinationLabel: (FfiBackupDestinationView) -> String = { it.displayName ?: it.destinationNestUrl },
    // ── The client-device destination kind (backups.md § Third destination kind) ──
    // Every seam below is injected for the same reason lastUploadText is: it
    // resolves through shared Rust over FFI, which the Robolectric harness cannot
    // load. The DEFAULTS are FFI-free stand-ins, never a second implementation of
    // the rule — the production values are wired in BackupDestinationsSection.
    /** Enroll this device as a custodian: (name, capacityCapBytes-or-null). */
    onEnrollCustodian: (String, ULong?) -> Unit = { _, _ -> },
    /** The shared kind catalog, in paint order (nest first). */
    kindOptions: List<DestinationKindOption> = emptyList(),
    /** `backup-destination-kind-badge` text for a row's own discriminator. */
    kindBadgeText: (FfiBackupDestinationView) -> String = { it.kind },
    /** `backup-destination-usage` text: held vs cap, cap-reached READ from the status. */
    usageText: (FfiBackupDestinationView, FfiBackupDestinationStatus?) -> String = { _, _ -> "" },
    /** Is this row one of the owner's own devices? The shared typed rule, never a string match. */
    isClientDevice: (FfiBackupDestinationView) -> Boolean = { false },
    /** Is EVERY configured destination a client device? The shared policy predicate. */
    soleClientDestinations: Boolean = false,
    /** The shared `parse_byte_size`; `null` is a REFUSAL to surface, never a default. */
    parseCapacity: (String) -> ULong? = { null },
    /**
     * Report a refusal to this page's ONE `error-message` surface — the
     * shell-level banner (FaunaNavHost) this section already errors through.
     * A second node tagged `error-message` inside the form would be a duplicate
     * id; linux writes the same refusal to its page-level `error_label`.
     */
    onError: (String) -> Unit = {},
    // ── Reclaim this device's copy (backups.md § Manage backup destinations) ──
    /**
     * Bytes this device holds with no destination row claiming them, or `null`
     * when `backup-orphaned-store-row` must not paint.
     *
     * ⚠ **A verdict, not a measurement** — the caller has already run the shared
     * `custodian_store_is_orphaned` over its own disk read and its own device
     * id. This composable must never re-derive it: the rule refuses in two
     * directions no render path can see, and it gates a gesture that destroys
     * the owner's only offline copy.
     */
    orphanedStoreBytes: ULong? = null,
    /** `backup-orphaned-store-row` sentence for those bytes, via the shared face. */
    orphanedStoreText: (ULong) -> String = { "" },
    /** Free this device's whole sealed store — the confirmed reclaim. */
    onReclaimStore: () -> Unit = {},
) {
    var form by remember { mutableStateOf<FormMode>(FormMode.Closed) }
    var removing by remember { mutableStateOf<FfiBackupDestinationView?>(null) }
    var confirmingReclaim by remember { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.backups_backup_destinations_title),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            stringResource(R.string.backups_backup_destinations_desc),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Button(
            onClick = { form = FormMode.Add },
            enabled = !working,
            modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_ADD_BUTTON),
        ) { Text(stringResource(R.string.backups_backup_destination_add_button)) }

        // ── Add / edit form (inline reveal, mirrors linux + MailAliasesContent) ──
        when (val mode = form) {
            is FormMode.Add -> DestinationForm(
                editing = null,
                working = working,
                kindOptions = kindOptions,
                parseCapacity = parseCapacity,
                onError = onError,
                onSubmit = { url, name -> onAdd(url, name); form = FormMode.Closed },
                onSubmitCustodian = { name, cap -> onEnrollCustodian(name, cap); form = FormMode.Closed },
                onCancel = { form = FormMode.Closed },
            )
            is FormMode.Edit -> DestinationForm(
                editing = mode.view,
                working = working,
                kindOptions = kindOptions,
                parseCapacity = parseCapacity,
                onError = onError,
                onSubmit = { url, name -> onEdit(mode.view.destinationId, url, name); form = FormMode.Closed },
                // Unreachable while editing: the kind select is painted disabled
                // and never read back, because the kind is not an editable
                // property (backups.md § Create / edit / remove).
                onSubmitCustodian = { _, _ -> },
                onCancel = { form = FormMode.Closed },
            )
            FormMode.Closed -> {}
        }

        // The sole-client durability warning, above the rows it is about and
        // painted only while true (the `backup-audit-alert` idiom; linux places
        // it identically). Deliberately NOT an alert: nothing is failing — the
        // durability story is just weaker than the user may assume
        // (backups.md § Third destination kind → Durability + labeling).
        if (soleClientDestinations) {
            Text(
                stringResource(R.string.backups_backup_sole_client_destination_warning),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.tertiary,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.BACKUP_SOLE_CLIENT_DESTINATION_WARNING),
            )
        }

        // ── Remove-confirm (inline reveal) ──
        removing?.let { dest ->
            RemoveConfirm(
                label = destinationLabel(dest),
                // The opt-in is offered on client-device rows ONLY, through the
                // shared typed predicate rather than a raw `kind` compare: an
                // Inert row owns no local store, and a kind this build does not
                // implement is not one of the owner's devices.
                offerReclaim = isClientDevice(dest),
                onConfirm = { alsoReclaim -> onRemove(dest.destinationId, alsoReclaim); removing = null },
                onCancel = { removing = null },
            )
        }

        // ── Reclaim this device's copy (backups.md § Manage backup destinations
        // → *Reclaim this device's copy*) ──
        // The row is what keeps the gesture reachable for a store whose
        // destination was already removed; without it the space is unrecoverable
        // from the app. Painted purely from the caller's cached verdict — see
        // `orphanedStoreBytes` for why it is never recomputed here.
        orphanedStoreBytes?.let { held ->
            Card(
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.BACKUP_ORPHANED_STORE_ROW),
            ) {
                Column(
                    modifier = Modifier.padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(orphanedStoreText(held), style = MaterialTheme.typography.bodySmall)
                    Button(
                        onClick = { confirmingReclaim = true },
                        enabled = !working,
                        modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_RECLAIM_BUTTON),
                    ) { Text(stringResource(R.string.backups_backup_destination_reclaim_button)) }
                }
            }
        }

        if (confirmingReclaim) {
            ReclaimConfirm(
                onConfirm = { onReclaimStore(); confirmingReclaim = false },
                onCancel = { confirmingReclaim = false },
            )
        }

        if (destinations.isEmpty()) {
            Text(
                stringResource(R.string.backups_backup_destinations_empty),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            destinations.forEach { dest ->
                DestinationRow(
                    dest = dest,
                    status = statuses[dest.destinationId],
                    working = working,
                    lastUploadText = lastUploadText,
                    backlogText = backlogText,
                    auditRow = auditRows[dest.destinationId],
                    lastAuditText = lastAuditText,
                    selfAuditText = selfAuditText,
                    destinationLabel = destinationLabel,
                    kindBadgeText = kindBadgeText,
                    usageText = usageText,
                    isClientDevice = isClientDevice,
                    onEdit = { form = FormMode.Edit(dest) },
                    onRemove = { removing = dest },
                )
            }
        }
    }
}

/**
 * The remove-confirm inline reveal (`backup-destination-remove-confirm-modal`).
 *
 * The offline gate's canonical pairing on this page (W4 (account-data-plane.md § Workstreams) phase 4,
 * `account-data-plane.md` § The offline-mutation contract): the confirm issues
 * `fauna.backup.destination.remove` — `OnlineOnly`, because the teardown runs
 * at the destination nest *and* the source nest's registry — while the cancel
 * beside it is pure local UI that must stay live with no nest at all. A gate
 * that greyed both would satisfy a one-sided check and break the contract,
 * which is why the two sit here declared side by side.
 */
@Composable
private fun RemoveConfirm(
    label: String,
    offerReclaim: Boolean,
    onConfirm: (Boolean) -> Unit,
    onCancel: () -> Unit,
) {
    val confirmGate = faunaGate("fauna.backup.destination.remove")
    // Opt-IN, so it starts unticked: removing a client-device destination
    // deliberately KEEPS the local sealed store (§ 3c-ii — it is the owner's
    // only offline copy), and a pre-ticked box would make deleting it the
    // default for anyone who did not read the line.
    var alsoReclaim by remember { mutableStateOf(false) }
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResource(R.string.backups_backup_destination_remove_confirm_title),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(label, style = MaterialTheme.typography.bodySmall)
            if (offerReclaim) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Checkbox(
                        checked = alsoReclaim,
                        onCheckedChange = { alsoReclaim = it },
                        modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_REMOVE_RECLAIM_CHECKBOX),
                    )
                    Text(
                        stringResource(R.string.backups_backup_destination_remove_reclaim_checkbox),
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onConfirm(alsoReclaim) },
                    enabled = confirmGate.enabled,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_REMOVE_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_destination_remove_confirm_button)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_REMOVE_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_destination_remove_cancel_button)) }
            }
            DisabledControlReasonText(confirmGate.reason)
        }
    }
}

/**
 * The reclaim-confirm inline reveal (`backup-reclaim-confirm-modal`).
 *
 * A **plain** confirm — no re-type, deliberately (`backups.md` § Manage backup
 * destinations → *Reclaim this device's copy*): the store is re-buildable from a
 * fresh pull whenever the device re-enrolls, so the friction of a typed
 * confirmation would overstate what is being lost. The modal exists at all
 * because reclaiming ends this device's ability to restore on its own with no
 * nest reachable — which the body text is the only place the user is told.
 *
 * The same offline pairing the remove-confirm declares: the confirm frees bytes
 * this device holds locally, but it is reached only from a page whose state the
 * nest supplies, and the cancel beside it is pure local UI that must stay live.
 */
@Composable
private fun ReclaimConfirm(
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_RECLAIM_CONFIRM_MODAL)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResource(R.string.backups_backup_reclaim_confirm_title),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                stringResource(R.string.backups_backup_reclaim_confirm_body),
                style = MaterialTheme.typography.bodySmall,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = onConfirm,
                    modifier = Modifier.testTag(Ids.BACKUP_RECLAIM_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_reclaim_confirm_button)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.BACKUP_RECLAIM_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_reclaim_cancel_button)) }
            }
        }
    }
}

/**
 * One `backup-destination-status-row`. The LIVE status maps onto the per-row IDs by
 * `destination_id` (backups.md § Per-destination status read): `last_upload_time` /
 * `backlog_count` render through [lastUploadText]/[backlogText] (the shared
 * `backupLastUploadLabel`/`backupBacklogLabel` never-vs-real + epoch-0-guard +
 * baseline decisions — no hand-rolled null-check or seconds handling here). Uniform
 * with linux/apple/windows; `last_upload_time` stays "never" until a per-app
 * always-on upload coordinator runs.
 */
@Composable
private fun DestinationRow(
    dest: FfiBackupDestinationView,
    status: FfiBackupDestinationStatus?,
    working: Boolean,
    lastUploadText: (FfiBackupDestinationStatus?) -> String,
    backlogText: (FfiBackupDestinationStatus?) -> String,
    auditRow: FfiDestinationAuditRow?,
    lastAuditText: (FfiDestinationAuditRow?) -> String,
    selfAuditText: (FfiBackupDestinationStatus?) -> String,
    destinationLabel: (FfiBackupDestinationView) -> String,
    kindBadgeText: (FfiBackupDestinationView) -> String,
    usageText: (FfiBackupDestinationView, FfiBackupDestinationStatus?) -> String,
    isClientDevice: (FfiBackupDestinationView) -> Boolean,
    onEdit: () -> Unit,
    onRemove: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_DESTINATION_STATUS_ROW)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(destinationLabel(dest), style = MaterialTheme.typography.titleSmall)
            // kind-badge — the visible half of "a client custodian never silently
            // satisfies *you have an off-site backup*" (backups.md § Durability +
            // labeling). Reads the row's own discriminator through the shared
            // label, so a kind a newer client wrote renders as itself rather than
            // masquerading as a nest.
            Text(
                kindBadgeText(dest),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_KIND_BADGE),
            )
            // usage — client-device rows only (ui.yaml). A nest row has no cap and
            // no held-bytes report, so the element is ABSENT rather than empty.
            if (isClientDevice(dest)) {
                Text(
                    usageText(dest, status),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_USAGE),
                )
            }
            Text(
                lastUploadText(status),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_LAST_UPLOAD_TIME),
            )
            Text(
                backlogText(status),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_BACKLOG_COUNT),
            )
            // last-audit-time — this client's OWN independent check for a nest
            // row, "never" until one passes (backups.md § Audit-alert
            // surface); a client-device row carries its own self-audit
            // instead, dispatched by kind rather than re-derived. Not the row
            // above: that one is the source nest reporting on its own
            // uploads; this is the only line on the page neither the source
            // nor the destination gets to assert.
            Text(
                if (isClientDevice(dest)) selfAuditText(status) else lastAuditText(auditRow),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_LAST_AUDIT_TIME),
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedButton(
                    onClick = onEdit,
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_EDIT_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_destination_edit_button)) }
                OutlinedButton(
                    onClick = onRemove,
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_REMOVE_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_destination_remove_button)) }
            }
        }
    }
}

/**
 * The add/edit destination form (`backup-destination-add-modal`, an inline
 * reveal). On Add, the fields start blank (a blank name defaults to the
 * destination domain / the device id server-side). On Edit, they prefill from
 * the row; a different-nest URL is rejected by the shared FFI (surfaced in the
 * error banner).
 *
 * **Both implemented destination kinds live in this one form** (backups.md
 * § Third destination kind), and `backup-destination-kind-select` **swaps** the
 * per-kind fields rather than disabling them: a custodian has no address at all,
 * so painting an empty URL box for it would invite the user to type one nothing
 * could ever use. In **edit** mode the select is painted *disabled* rather than
 * hidden and is never read back — the kind is not an editable property, and
 * re-pointing a live row would keep a `destination_id` whose registry row and
 * grants describe the other kind. Same shape as tui's and linux's landed legs.
 */
@Composable
private fun DestinationForm(
    editing: FfiBackupDestinationView?,
    working: Boolean,
    kindOptions: List<DestinationKindOption>,
    parseCapacity: (String) -> ULong?,
    onSubmit: (String, String) -> Unit,
    onSubmitCustodian: (String, ULong?) -> Unit,
    onError: (String) -> Unit,
    onCancel: () -> Unit,
) {
    val capacityInvalidMessage = stringResource(R.string.backups_backup_destination_capacity_invalid)
    var url by remember(editing) { mutableStateOf(editing?.destinationNestUrl ?: "") }
    var name by remember(editing) { mutableStateOf(editing?.displayName ?: "") }
    var capacity by remember(editing) { mutableStateOf("") }
    var capacityError by remember(editing) { mutableStateOf(false) }
    // The selected wire discriminator. On edit it is the row's own kind (the
    // control is disabled, so this only decides what it *shows*); on add it is
    // the catalog's first entry — nest, which the shared catalog paints first
    // because it is the kind that actually satisfies "off-site".
    var kind by remember(editing) {
        mutableStateOf(editing?.kind ?: kindOptions.firstOrNull()?.value.orEmpty())
    }
    // An unrecognised kind (a row a newer client wrote) matches no option, so
    // this is false and the form shows the nest fields — but edit mode never
    // reads the select back, so such a row cannot be rewritten by what it shows.
    val custodian = kindOptions.firstOrNull { it.value == kind }?.isClientDevice == true

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_DESTINATION_ADD_MODAL)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResource(
                    if (editing == null) R.string.backups_backup_destination_form_add_title
                    else R.string.backups_backup_destination_form_edit_title,
                ),
                style = MaterialTheme.typography.titleSmall,
            )

            // The kind select, above the fields it swaps. Its caption is plain
            // text rather than a tagged element — ui.yaml scopes the id to the
            // control itself.
            Text(
                stringResource(R.string.backups_backup_destination_kind_select_label),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // The token-round-tripping select every raw-value picker on this
            // app shares: the driver reads/picks the wire value, the human the
            // label. Disabled in EDIT mode — not hidden, and never read back.
            TokenSelect(
                testTagValue = Ids.BACKUP_DESTINATION_KIND_SELECT,
                selected = kind,
                options = kindOptions.map { it.value to it.label },
                enabled = editing == null && !working,
                onSelect = { value ->
                    kind = value
                    capacityError = false
                },
            )

            // The URL is the NEST kind's field (ui.yaml: "nest kind only").
            if (!custodian) {
                OutlinedTextField(
                    value = url,
                    onValueChange = { url = it },
                    label = { Text(stringResource(R.string.backups_backup_destination_url_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_DESTINATION_URL_INPUT),
                )
            }
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.backups_backup_destination_name_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_DESTINATION_NAME_INPUT),
            )
            // The capacity cap — the client-device kind's only knob, so it paints
            // for that kind alone (ui.yaml: "this-device kind only").
            if (custodian) {
                OutlinedTextField(
                    value = capacity,
                    onValueChange = { capacity = it; capacityError = false },
                    label = { Text(stringResource(R.string.backups_backup_destination_capacity_placeholder)) },
                    isError = capacityError,
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.BACKUP_DESTINATION_CAPACITY_INPUT),
                )
                // NB the refusal TEXT does not live here. This page's
                // `error-message` is the shell-level banner (FaunaNavHost), and a
                // second node carrying that id would be a duplicate — so an
                // unreadable cap is reported through [onError] to that one
                // surface, exactly as linux writes it to its page-level
                // error_label. `isError` above is the field's own local styling.
                // The honest statement backups.md § Threat model requires AT
                // OPT-IN: a complete offline corpus is a materially different
                // exposure from an ordinary logged-in device, and the user must
                // see it here rather than discover it later.
                Text(
                    stringResource(R.string.backups_backup_destination_custodian_exposure),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            // ⚠ A DISCRIMINANT: the two branches are two ceremonies reaching two
            // FFI verbs, and each declares the kind that BINDS it — the ruling tui
            // states as the lead app (`apps/fauna-tui/src/backups.rs` wire_kind,
            // `SubmitTarget::{AddNest,AddCustodian}`), and which this app's own
            // call chain confirms. The nest path (`backupDestinationAdd` →
            // `backup_destination_add`) opens an authenticated session to the
            // destination and registers the nest-writer grant over it, so its
            // first mandatory call is `fauna.backup.nest_key.grant`; the config
            // write that ENDS the ceremony is `OfflineSafe`, and declaring that
            // one would leave a control live that cannot possibly finish. The
            // custodian path (`backupDestinationEnrollCustodian`) has no address
            // to resolve and — deliberately — no key grant at all, so it binds on
            // its registry write, `fauna.backup.destination.register`.
            //
            // ⚠ And it is THREE-way, not two: this composable also serves the
            // EDIT form, whose submit is `onEdit`, a rename / re-point of one row
            // of the owner's own config document — `fauna.account.state.put`, OfflineSafe,
            // so it must stay LIVE with no nest. That is tui's `SubmitTarget::Edit`
            // ruling, and this app's FFI says the same in as many words
            // (`backup_destination_edit`: "renaming an offline destination must
            // still work" — it re-resolves only when the URL actually changed).
            // Declaring an OnlineOnly kind here would grey exactly the edit the
            // shared layer went out of its way to keep working offline.
            //
            // The gate is handed the SAME expression the click branches on
            // (`custodian && editing == null`, then edit-vs-add), so the kind and
            // the action cannot drift. The two add kinds are both OnlineOnly
            // today, so for them this changes no pixel — but the REASON the button
            // paints is now the right one, and the moment the nest ceremony's tail
            // behaves as tui describes, the correct half is what the gate reasons
            // about.
            //
            // The page's own predicate is handed over verbatim; note the
            // custodian carve-out inside it (a custodian has no URL, so the
            // nest kind's non-blank-URL gate must not apply to it — it would
            // disable the button forever on a form that never shows a URL box).
            val registerGate = faunaGate(
                when {
                    editing != null -> "fauna.account.state.put"
                    custodian -> "fauna.backup.destination.register"
                    else -> "fauna.backup.nest_key.grant"
                },
                enabled = !working && (custodian || url.isNotBlank()),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        if (custodian && editing == null) {
                            // A blank cap is a real choice: null is uncapped. A
                            // non-blank one that cannot be read is a REFUSAL the
                            // user sees, never a substituted default — guessing a
                            // cap is how a device's disk fills.
                            val typed = capacity.trim()
                            if (typed.isEmpty()) {
                                onSubmitCustodian(name, null)
                            } else {
                                val parsed = parseCapacity(typed)
                                if (parsed == null) {
                                    capacityError = true
                                    onError(capacityInvalidMessage)
                                } else {
                                    onSubmitCustodian(name, parsed)
                                }
                            }
                        } else {
                            onSubmit(url, name)
                        }
                    },
                    enabled = registerGate.enabled,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_ADD_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_destination_add_confirm)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.BACKUP_DESTINATION_ADD_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.backups_backup_destination_add_cancel)) }
            }
            DisabledControlReasonText(registerGate.reason)
        }
    }
}
