package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.navigation.exitAdminToPrimary
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.and
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.faunaReadGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.paintedReason
import com.fauna.app.ui.util.faunaFormGate
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.formRefusal
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.isShown
import com.fauna.app.ui.viewmodel.AdminSettingsVM
import com.fauna.ffi.FfiAdminMembershipTier
import com.fauna.ffi.FfiAdminTier
import com.fauna.ffi.defaultLapseTier
import uniffi.fauna_core.Possibility
import social.fauna.generated.Ids
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.DropdownMenuItem
import com.fauna.app.ui.controls.OutlinedButton
import com.fauna.app.ui.controls.OutlinedTextField
import com.fauna.app.ui.controls.ExposedDropdownMenuBox

/**
 * The `admin-settings` page — nav-labelled **"Tiers"** after the per-page-services
 * redesign (admin.md § 3 Settings / § Admin IA redesign): tier *definitions* only,
 * with in-place cap editing. The factory-reset danger zone **moved to the new
 * `admin-nest` page** (admin.md § N Nest) — see [AdminNestScreen]; the read-only
 * `nest-mode-indicator` that also lived there is retired outright (there is no
 * storage mode left to indicate, storage-modes.md). The element IDs keep their
 * historical `admin-settings-` prefix. Drives `fauna.admin.tiers.*` via
 * [AdminSettingsVM] → `FfiAdminClient`.
 *
 * Stateless [AdminSettingsContent] is split out so it renders under the Compose
 * test harness with seeded state — the VM-bound [AdminSettingsScreen] is the
 * thin wrapper the navigation graph mounts. Mirrors the Linux reference
 * (`apps/fauna-linux/src/views/admin.rs` `build_settings_page`) and Windows
 * `AdminSettingsPage` / `AdminSettingsViewModel`.
 *
 * The android admin pages are NavHost routes (admin.md § Navigation model lists
 * the cross-app shell-unification as a separate gap); `admin-nav-back` here
 * pops back to the admin dashboard, matching the sibling `admin-users` page.
 */
@Composable
fun AdminSettingsScreen(
    navController: NavController,
    vm: AdminSettingsVM = hiltViewModel(),
) {
    val tiers by vm.tiers.collectAsState()
    val ownMembershipTierNames by vm.ownMembershipTierNames.collectAsState()
    val membershipTiers by vm.membershipTiers.collectAsState()
    val actionError by vm.actionError.collectAsState()
    val tierAddGeneration by vm.tierAddGeneration.collectAsState()
    val addTierAnswer by vm.addTierAnswer.collectAsState()

    LaunchedEffect(Unit) { vm.refresh() }

    // Route tier-save failures to the app-wide error-message banner
    // (LocalAppMessages), matching the Devices page — admin-settings has no
    // dedicated per-page error element in ui.yaml.
    val appMessages = LocalAppMessages.current
    LaunchedEffect(actionError) { actionError?.let { appMessages.showError(it) } }

    AdminSettingsContent(
        tiers = tiers,
        ownMembershipTierNames = ownMembershipTierNames,
        membershipTiers = membershipTiers,
        onBack = { navController.exitAdminToPrimary() },
        onSaveTier = vm::saveTier,
        onDeleteTier = vm::deleteTier,
        onAddTier = vm::addTier,
        addFormGeneration = tierAddGeneration,
        onRefused = appMessages::showError,
        onSaveMembershipTier = vm::saveMembershipTier,
        onClearMembershipTier = vm::clearMembershipTier,
        addTierAnswer = addTierAnswer,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminSettingsContent(
    tiers: List<FfiAdminTier>,
    ownMembershipTierNames: List<String> = emptyList(),
    membershipTiers: List<FfiAdminMembershipTier> = emptyList(),
    onBack: () -> Unit,
    onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit,
    // Fires with the row's tier name; the VM removes it and refetches.
    onDeleteTier: (String) -> Unit = {},
    // The add-a-tier form: `onAddTier` fires with the trimmed name + the five
    // parsed caps; `addFormGeneration` bumps on a successful create so the
    // drafts clear only then. `onRefused` carries a refused press's reason
    // (a tier row's save, the add form, a membership row's save — each asking
    // its shared draft answer again) to the page's `error-message` banner.
    onAddTier: (String, Long, Long, Long, Long, Long) -> Unit = { _, _, _, _, _, _ -> },
    addFormGeneration: Int = 0,
    onRefused: (String) -> Unit = {},
    onSaveMembershipTier: (String, String, String) -> Unit = { _, _, _ -> },
    onClearMembershipTier: (String) -> Unit = {},
    // Tier-cap parsing via the shared `fauna_core::format::parse_cap` (trim,
    // parse i64, clamp ≥0) — run only once the draft answer below admitted the
    // caps, so it never falls back to the persisted value. Injected as an
    // FFI-free lambda so the Compose test stays off the native path.
    // value-formatting.md § Tier cap validation.
    parseCap: (String) -> Long? = { com.fauna.ffi.parseCap(it) },
    // The forms' draft answers (admin.md § Errors & edge cases, the form
    // bullet): `fauna_client_admin::{tier_caps_possibility,
    // tier_create_possibility, membership_save_possibility}`. Each submit is
    // painted from its answer and asks it again at the press; fail open off
    // the native path ([askForm]).
    drafts: AdminSettingsDrafts = AdminSettingsDrafts(),
    // `fauna_client_admin::tier_delete_possibility` (admin.md § 3 → Removing a
    // tier) — absent on `free`; injected FFI-free like [parseCap].
    tierDeletePossibility: (String) -> Possibility = { com.fauna.ffi.adminTierDeletePossibility(it) },
    // `fauna_client_admin::membership_clear_possibility` (admin.md § 3,
    // membership designations) — blocked, saying nothing is set, on a row with
    // no persisted designation; fails open off the native path ([askForm]).
    membershipClearPossibility: (Boolean) -> Possibility = {
        askForm { com.fauna.ffi.adminMembershipClearPossibility(it) }
    },
    // The tier add form's answer — the shared `write_after_read_possibility`
    // over the page's own `tiers.list` read (admin.md § Errors & edge cases,
    // *Controls wait for their read*): blocked still loading before it,
    // failed to load after a refused first read, still saving while a create
    // is on its way. Folded FIRST, before the form's draft answer.
    addTierAnswer: Possibility = Possibility.Possible,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_settings_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_HEADING),
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
                .padding(horizontal = 16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // ── Tier definitions (in-place cap editing, admin.md § 3) ──
            TiersSection(tiers, onSaveTier, onDeleteTier, parseCap, tierDeletePossibility, drafts, onRefused)
            TierAddForm(onAddTier, addFormGeneration, onRefused, parseCap, drafts, addTierAnswer)
            // ── Membership designations (monetization.md § Pillar 4) ──
            MembershipSection(
                quotaTierNames = tiers.map { it.name },
                ownMembershipTierNames = ownMembershipTierNames,
                membershipTiers = membershipTiers,
                onSave = onSaveMembershipTier,
                onClear = onClearMembershipTier,
                clearPossibility = membershipClearPossibility,
                saveDraft = drafts.membershipSave,
                onRefused = onRefused,
            )
        }
    }
}

/**
 * The tiers page's draft-validity answers, each a shared `fauna_client_admin`
 * predicate over UniFFI, injected FFI-free like [AdminUsersAnswers] so the
 * Robolectric harness stays off the native path.
 */
class AdminSettingsDrafts(
    // The five caps in the row order: inbox, storage, devices, blob size, feeds.
    val tierCaps: (List<String>) -> Possibility = { askForm { com.fauna.ffi.adminTierCapsPossibility(it) } },
    val tierCreate: (name: String, caps: List<String>) -> Possibility = { name, caps ->
        askForm { com.fauna.ffi.adminTierCreatePossibility(name, caps) }
    },
    val membershipSave: (adminTier: String) -> Possibility = {
        askForm { com.fauna.ffi.adminMembershipSavePossibility(it) }
    },
)

/** Ask [answer] again at a press: `true` (and its reason sent to [onRefused])
 *  when the press must send nothing. */
private fun refused(context: android.content.Context, answer: Possibility, onRefused: (String) -> Unit): Boolean {
    val refusal = formRefusal(context, answer) ?: return false
    refusal.reason?.let(onRefused)
    return true
}

// ── Tiers ─────────────────────────────────────────────────────────────────────

@Composable
private fun TiersSection(
    tiers: List<FfiAdminTier>,
    onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit,
    onDeleteTier: (String) -> Unit,
    parseCap: (String) -> Long?,
    tierDeletePossibility: (String) -> Possibility,
    drafts: AdminSettingsDrafts,
    onRefused: (String) -> Unit,
) {
    Column(modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_TIERS_SECTION)) {
        Text(
            stringResource(R.string.admin_settings_page_tiers),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp),
        )
        if (tiers.isEmpty()) {
            Text(
                stringResource(R.string.admin_settings_page_no_tiers),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            tiers.forEach { tier ->
                TierRow(tier, onSaveTier, onDeleteTier, parseCap, tierDeletePossibility, drafts, onRefused)
            }
        }
    }
}

@Composable
private fun TierRow(
    tier: FfiAdminTier,
    onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit,
    onDeleteTier: (String) -> Unit,
    parseCap: (String) -> Long?,
    tierDeletePossibility: (String) -> Possibility,
    drafts: AdminSettingsDrafts,
    onRefused: (String) -> Unit,
) {
    val context = LocalContext.current
    // Each field pre-fills with the persisted value; an empty or unparseable
    // edit blocks the save with the shared reason — never replaced by the
    // persisted value (admin.md § Errors & edge cases, the form bullet).
    var inbox by remember(tier) { mutableStateOf(tier.maxInboxBytes.toString()) }
    var storage by remember(tier) { mutableStateOf(tier.maxStorageBytes.toString()) }
    var devices by remember(tier) { mutableStateOf(tier.maxDevices.toString()) }
    var blobSize by remember(tier) { mutableStateOf(tier.maxBlobSize.toString()) }
    var feeds by remember(tier) { mutableStateOf(tier.maxFeeds.toString()) }

    Card(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag(Ids.ADMIN_SETTINGS_TIER_ITEM)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(tier.name, style = MaterialTheme.typography.titleSmall)
            CapField(R.string.admin_settings_page_cap_inbox_bytes, "admin-settings-tier-cap-inbox", inbox) { inbox = it }
            CapField(R.string.admin_settings_page_cap_storage_bytes, "admin-settings-tier-cap-storage", storage) { storage = it }
            CapField(R.string.admin_settings_page_cap_devices, "admin-settings-tier-cap-devices", devices) { devices = it }
            CapField(R.string.admin_settings_page_cap_blob_size, "admin-settings-tier-cap-blob-size", blobSize) { blobSize = it }
            CapField(R.string.admin_settings_page_cap_feeds, "admin-settings-tier-cap-feeds", feeds) { feeds = it }
            // The five cap fields above are local drafts; this one Save
            // overwrites all five in one `fauna.admin.tiers.update` call, so the
            // Save carries the card's whole declaration.
            val caps = { listOf(inbox, storage, devices, blobSize, feeds) }
            val capsAnswer = drafts.tierCaps(caps())
            val saveTierGate = faunaFormGate(
                capsAnswer,
                faunaGate("fauna.admin.tiers.update", enabled = capsAnswer.isPossible),
            )
            Button(
                onClick = {
                    if (refused(context, drafts.tierCaps(caps()), onRefused)) return@Button
                    val parsed = caps().map(parseCap)
                    if (parsed.any { it == null }) return@Button
                    onSaveTier(tier.name, parsed[0]!!, parsed[1]!!, parsed[2]!!, parsed[3]!!, parsed[4]!!)
                },
                enabled = saveTierGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_TIER_SAVE_BUTTON),
            ) { Text(stringResource(R.string.admin_settings_page_save_tier)) }
            DisabledControlReasonText(saveTierGate.reason)
            // Painted from the shared answer (admin.md § 3 → *Removing a
            // tier*): absent on `free`, which the nest always refuses; a
            // still-referenced tier's refusal reads on the error line. No
            // confirm; the click asks again. tui's `admin::Action::DeleteTier`.
            if (tierDeletePossibility(tier.name).isShown) {
                val deleteTierGate = faunaGate("fauna.admin.tiers.delete")
                OutlinedButton(
                    onClick = {
                        if (tierDeletePossibility(tier.name).isPossible) onDeleteTier(tier.name)
                    },
                    enabled = deleteTierGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_TIER_DELETE_BUTTON),
                ) { Text(stringResource(R.string.admin_settings_page_delete_tier)) }
                DisabledControlReasonText(deleteTierGate.reason)
            }
        }
    }
}

/**
 * The add-a-tier form (`admin-settings-tier-add-section`, admin.md § 3 →
 * *Defining a new tier*), a sibling of the tier rows so a fresh box with no
 * tiers can still define one. The five cap fields reuse the row editor's ids,
 * scoped under the section. The submit is painted from the shared
 * `tier_create_possibility` (a name, and every cap through `parse_cap`) and
 * asks it again at the press, refusing with its reason; the drafts clear only
 * when [generation] bumps (the nest accepted the create).
 */
@Composable
private fun TierAddForm(
    onAddTier: (String, Long, Long, Long, Long, Long) -> Unit,
    generation: Int,
    onRefused: (String) -> Unit,
    parseCap: (String) -> Long?,
    drafts: AdminSettingsDrafts,
    readAnswer: Possibility,
) {
    val context = LocalContext.current
    // The name and caps are live only once the read resolved and no create is
    // on its way; each publishes the read's reason as its automation `reason`.
    val fieldsLive = readAnswer.isPossible
    val fieldReason = localized(readAnswer.blockedReason)
    var name by remember(generation) { mutableStateOf("") }
    var inbox by remember(generation) { mutableStateOf("") }
    var storage by remember(generation) { mutableStateOf("") }
    var devices by remember(generation) { mutableStateOf("") }
    var blobSize by remember(generation) { mutableStateOf("") }
    var feeds by remember(generation) { mutableStateOf("") }

    Card(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag(Ids.ADMIN_SETTINGS_TIER_ADD_SECTION)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(stringResource(R.string.admin_settings_page_add_tier_section), style = MaterialTheme.typography.titleSmall)
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.admin_settings_page_add_tier_name)) },
                singleLine = true,
                enabled = fieldsLive,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_SETTINGS_TIER_ADD_NAME_INPUT)
                    .paintedReason(fieldReason),
            )
            CapField(R.string.admin_settings_page_cap_inbox_bytes, "admin-settings-tier-cap-inbox", inbox, fieldsLive, fieldReason) { inbox = it }
            CapField(R.string.admin_settings_page_cap_storage_bytes, "admin-settings-tier-cap-storage", storage, fieldsLive, fieldReason) { storage = it }
            CapField(R.string.admin_settings_page_cap_devices, "admin-settings-tier-cap-devices", devices, fieldsLive, fieldReason) { devices = it }
            CapField(R.string.admin_settings_page_cap_blob_size, "admin-settings-tier-cap-blob-size", blobSize, fieldsLive, fieldReason) { blobSize = it }
            CapField(R.string.admin_settings_page_cap_feeds, "admin-settings-tier-cap-feeds", feeds, fieldsLive, fieldReason) { feeds = it }
            val caps = { listOf(inbox, storage, devices, blobSize, feeds) }
            val createDraft = drafts.tierCreate(name, caps())
            val createAnswer = readAnswer and createDraft
            val addTierGate = faunaReadGate(
                readAnswer,
                faunaGate("fauna.admin.tiers.create", enabled = createAnswer.isPossible),
                createDraft,
            )
            Button(
                onClick = {
                    if (refused(context, readAnswer and drafts.tierCreate(name, caps()), onRefused)) return@Button
                    val parsed = caps().map(parseCap)
                    if (parsed.any { it == null }) return@Button
                    onAddTier(name.trim(), parsed[0]!!, parsed[1]!!, parsed[2]!!, parsed[3]!!, parsed[4]!!)
                },
                enabled = addTierGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_TIER_ADD_BUTTON).paintedReason(addTierGate.reason),
            ) { Text(stringResource(R.string.admin_settings_page_add_tier)) }
            DisabledControlReasonText(addTierGate.reason)
        }
    }
}

@Composable
private fun CapField(
    labelRes: Int,
    testId: String,
    value: String,
    enabled: Boolean = true,
    // A blocked field's reason, published as its automation `reason`.
    reason: String? = null,
    onValueChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = { onValueChange(it.filter(Char::isDigit)) },
        label = { Text(stringResource(labelRes)) },
        singleLine = true,
        enabled = enabled,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(testId).paintedReason(reason),
    )
}

// ── Membership designations (monetization.md § Pillar 4) ───────────────────
//
// A link editor over the admin's own subscription tiers, never a third tier
// list: one admin-settings-membership-item row per subscription tier the
// admin owns, joining it to an admitted and a lapsed quota tier. Designating
// mutates neither tier system — it only records the link. Mirrors the Linux
// reference (`apps/fauna-linux/src/views/admin.rs` `update_membership_tiers` /
// `build_membership_row`) and the web `+page.svelte` leg.

@Composable
private fun MembershipSection(
    quotaTierNames: List<String>,
    ownMembershipTierNames: List<String>,
    membershipTiers: List<FfiAdminMembershipTier>,
    onSave: (String, String, String) -> Unit,
    onClear: (String) -> Unit,
    clearPossibility: (Boolean) -> Possibility,
    saveDraft: (String) -> Possibility,
    onRefused: (String) -> Unit,
) {
    Column(modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_MEMBERSHIP_SECTION)) {
        Text(
            stringResource(R.string.admin_settings_page_membership_section),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(vertical = 8.dp),
        )
        if (ownMembershipTierNames.isEmpty()) {
            // The normal out-of-the-box state (no subscription tiers minted
            // yet) — an empty-state pointer at the admin's own Tiers tab,
            // never an error (monetization.md § Pillar 4).
            Text(
                stringResource(R.string.admin_settings_page_no_membership_tiers),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            ownMembershipTierNames.forEach { name ->
                val existing = membershipTiers.find { it.tierName == name }
                MembershipRow(
                    name, existing, ownMembershipTierNames, quotaTierNames, onSave, onClear, clearPossibility,
                    saveDraft, onRefused,
                )
            }
        }
    }
}

@Composable
private fun MembershipRow(
    tierName: String,
    existing: FfiAdminMembershipTier?,
    ownMembershipTierNames: List<String>,
    quotaTierNames: List<String>,
    onSave: (String, String, String) -> Unit,
    onClear: (String) -> Unit,
    clearPossibility: (Boolean) -> Possibility,
    saveDraft: (String) -> Possibility,
    onRefused: (String) -> Unit,
) {
    val context = LocalContext.current
    // The tier-select is display + fix-up (a real select per the approved
    // shape, letting an admin correct a mis-set row without deleting it) —
    // Save reads the CURRENT selection, never the row's original identity
    // (mirrors the linux reference's `dropdown_tier(&tier_select)` at save
    // time). Clear always targets this row's own identity, `tierName`.
    var selectedTierName by remember(tierName) { mutableStateOf(tierName) }
    var adminTier by remember(existing) { mutableStateOf(existing?.adminTier ?: "") }
    var lapseTier by remember(existing) {
        mutableStateOf(existing?.lapseTier ?: defaultLapseTier())
    }

    Card(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag(Ids.ADMIN_SETTINGS_MEMBERSHIP_ITEM)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            MembershipTierPicker(
                testId = "admin-settings-membership-tier-select",
                labelRes = R.string.admin_settings_page_tiers,
                options = ownMembershipTierNames,
                selected = selectedTierName,
                onSelect = { selectedTierName = it },
            )
            MembershipTierPicker(
                testId = "admin-settings-membership-admin-tier-select",
                labelRes = R.string.admin_settings_page_membership_admits_at,
                options = quotaTierNames,
                selected = adminTier,
                onSelect = { adminTier = it },
            )
            MembershipTierPicker(
                testId = "admin-settings-membership-lapse-tier-select",
                labelRes = R.string.admin_settings_page_membership_lapses_to,
                options = quotaTierNames,
                selected = lapseTier,
                onSelect = { lapseTier = it },
            )
            // Save and Clear are DIFFERENT wire kinds (`membership_tiers.set` vs
            // `.clear`), so each declares its own — and Clear is painted from
            // the shared `membership_clear_possibility`: an undesignated row
            // has nothing to clear, said beside it. The three pickers above are
            // drafts and stay live.
            // Save is painted from the shared `membership_save_possibility`:
            // blocked, saying so, until an admitted-at tier is picked.
            val saveAnswer = saveDraft(adminTier)
            val membershipSaveGate = faunaFormGate(
                saveAnswer,
                faunaGate("fauna.admin.membership_tiers.set", enabled = saveAnswer.isPossible),
            )
            val clearAnswer = clearPossibility(existing != null)
            val membershipClearGate = faunaFormGate(
                clearAnswer,
                faunaGate("fauna.admin.membership_tiers.clear", enabled = clearAnswer.isPossible),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    // Asks the draft answer again: a blank admin-tier is
                    // refused with its reason, never sent for the nest to
                    // refuse with fauna.admin.invalid_params.
                    onClick = {
                        if (!refused(context, saveDraft(adminTier), onRefused)) {
                            onSave(selectedTierName, adminTier, lapseTier)
                        }
                    },
                    enabled = membershipSaveGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_MEMBERSHIP_SAVE_BUTTON),
                ) { Text(stringResource(R.string.admin_settings_page_membership_save)) }
                OutlinedButton(
                    onClick = { if (clearPossibility(existing != null).isPossible) onClear(tierName) },
                    enabled = membershipClearGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_MEMBERSHIP_CLEAR_BUTTON),
                ) { Text(stringResource(R.string.admin_settings_page_membership_clear)) }
            }
            DisabledControlReasonText(membershipSaveGate.reason ?: membershipClearGate.reason)
        }
    }
}

/**
 * The membership-row select — a native Compose `ExposedDropdownMenuBox`
 * (ui.yaml types `admin-settings-membership-*-select` as `select`), matching
 * the cross-section [TierDropdown] shape in `AdminUsersScreen.kt` but with a
 * per-field label. The anchor's display value is the selected option text so
 * the e2e `actions/admin.py` `_pick_tier` (`get_text` + `driver.select`) sees
 * the readable value.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MembershipTierPicker(
    testId: String,
    labelRes: Int,
    options: List<String>,
    selected: String,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = !expanded },
    ) {
        OutlinedTextField(
            value = selected,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(labelRes)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(testId),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(option) },
                    onClick = {
                        expanded = false
                        onSelect(option)
                    },
                )
            }
        }
    }
}
