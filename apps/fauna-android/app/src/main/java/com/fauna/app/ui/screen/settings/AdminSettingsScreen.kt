package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.AdminSettingsVM
import com.fauna.ffi.FfiAdminMembershipTier
import com.fauna.ffi.FfiAdminTier
import com.fauna.ffi.defaultLapseTier
import social.fauna.generated.Ids

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
        onBack = { navController.popBackStack() },
        onSaveTier = vm::saveTier,
        onSaveMembershipTier = vm::saveMembershipTier,
        onClearMembershipTier = vm::clearMembershipTier,
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
    onSaveMembershipTier: (String, String, String) -> Unit = { _, _, _ -> },
    onClearMembershipTier: (String) -> Unit = {},
    // Tier-cap validation via the shared `fauna_core::format::parse_cap` (trim,
    // parse i64, clamp ≥0); a `null` return ⇒ keep the persisted value. Injected
    // as an FFI-free lambda so the Compose test stays off the native path (mirrors
    // the `parsePort` admin-port leg). value-formatting.md § Tier cap validation.
    parseCap: (String) -> Long? = { com.fauna.ffi.parseCap(it) },
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
            TiersSection(tiers, onSaveTier, parseCap)
            // ── Membership designations (monetization.md § Pillar 4) ──
            MembershipSection(
                quotaTierNames = tiers.map { it.name },
                ownMembershipTierNames = ownMembershipTierNames,
                membershipTiers = membershipTiers,
                onSave = onSaveMembershipTier,
                onClear = onClearMembershipTier,
            )
        }
    }
}

// ── Tiers ─────────────────────────────────────────────────────────────────────

@Composable
private fun TiersSection(
    tiers: List<FfiAdminTier>,
    onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit,
    parseCap: (String) -> Long?,
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
            tiers.forEach { tier -> TierRow(tier, onSaveTier, parseCap) }
        }
    }
}

@Composable
private fun TierRow(
    tier: FfiAdminTier,
    onSaveTier: (String, Long, Long, Long, Long, Long) -> Unit,
    parseCap: (String) -> Long?,
) {
    // Each field pre-fills with the persisted value; an empty/unparseable edit
    // falls back to it at save (parseCap) so a stray edit never zeroes a cap.
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
            val saveTierGate = faunaGate("fauna.admin.tiers.update")
            Button(
                onClick = {
                    onSaveTier(
                        tier.name,
                        parseCap(inbox) ?: tier.maxInboxBytes,
                        parseCap(storage) ?: tier.maxStorageBytes,
                        parseCap(devices) ?: tier.maxDevices,
                        parseCap(blobSize) ?: tier.maxBlobSize,
                        parseCap(feeds) ?: tier.maxFeeds,
                    )
                },
                enabled = saveTierGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_TIER_SAVE_BUTTON),
            ) { Text(stringResource(R.string.admin_settings_page_save_tier)) }
            DisabledControlReasonText(saveTierGate.reason)
        }
    }
}

@Composable
private fun CapField(
    labelRes: Int,
    testId: String,
    value: String,
    onValueChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = { onValueChange(it.filter(Char::isDigit)) },
        label = { Text(stringResource(labelRes)) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = Modifier.fillMaxWidth().testTag(testId),
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
                MembershipRow(name, existing, ownMembershipTierNames, quotaTierNames, onSave, onClear)
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
) {
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
            // `.clear`), so each declares its own — and Clear keeps the page
            // predicate it already had: an undesignated row has nothing to
            // clear, and that stays true while connected. The three pickers
            // above are drafts and stay live.
            val membershipSaveGate = faunaGate("fauna.admin.membership_tiers.set")
            val membershipClearGate =
                faunaGate("fauna.admin.membership_tiers.clear", enabled = existing != null)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    // A blank admin-tier means nothing has been picked yet —
                    // no-op rather than sending a request the nest would
                    // refuse with fauna.admin.invalid_params (mirrors the
                    // linux reference's early-return).
                    onClick = { if (adminTier.isNotEmpty()) onSave(selectedTierName, adminTier, lapseTier) },
                    enabled = membershipSaveGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_SETTINGS_MEMBERSHIP_SAVE_BUTTON),
                ) { Text(stringResource(R.string.admin_settings_page_membership_save)) }
                OutlinedButton(
                    onClick = { onClear(tierName) },
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
